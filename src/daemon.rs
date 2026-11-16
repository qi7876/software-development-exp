use std::{error::Error, fs::File, path::Path};

pub(crate) fn lock_directory(directory: &Path) -> Result<File, Box<dyn Error>> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join("server.lock"))?;
    file.try_lock().map_err(|error| {
        format!(
            "working directory {} is already in use or cannot be locked: {error}",
            directory.display()
        )
    })?;
    Ok(file)
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::{
        io,
        net::SocketAddr,
        os::unix::{
            fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
            process::CommandExt,
        },
        path::PathBuf,
        process::{Command, Stdio},
        time::Duration,
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::{UnixListener, UnixStream},
        sync::watch,
        time::{Instant, sleep, timeout},
    };

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Status {
        pid: u32,
        working_directory: PathBuf,
        address: SocketAddr,
        stopping: bool,
    }

    fn state_directory() -> Result<PathBuf, Box<dyn Error>> {
        if let Some(path) = std::env::var_os("XDG_STATE_HOME").filter(|p| !p.is_empty()) {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err("XDG_STATE_HOME must be absolute".into());
            }
            return Ok(path.join("bak"));
        }
        Ok(PathBuf::from(
            std::env::var_os("HOME")
                .ok_or("HOME or XDG_STATE_HOME is required for background server commands")?,
        )
        .join(".local/state/bak"))
    }

    async fn request(command: &str) -> Result<Option<Status>, Box<dyn Error>> {
        let path = state_directory()?.join("server.sock");
        let exchange = async {
            let mut stream = match UnixStream::connect(&path).await {
                Ok(stream) => stream,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error.into()),
            };
            stream.write_all(command.as_bytes()).await?;
            stream.write_all(b"\n").await?;
            let mut response = String::new();
            BufReader::new(stream).read_line(&mut response).await?;
            Ok::<_, Box<dyn Error>>(Some(serde_json::from_str(&response)?))
        };
        timeout(Duration::from_secs(3), exchange)
            .await
            .map_err(|_| "server control request timed out")?
    }

    fn describe(status: &Status) {
        println!(
            "{} (PID {})\nWorking directory: {}\nListening on {}",
            if status.stopping {
                "Stopping"
            } else {
                "Running"
            },
            status.pid,
            status.working_directory.display(),
            status.address
        );
    }

    pub(crate) async fn status() -> Result<(), Box<dyn Error>> {
        match request("status").await? {
            Some(status) => describe(&status),
            None => println!("Server is stopped"),
        }
        Ok(())
    }

    pub(crate) async fn stop() -> Result<(), Box<dyn Error>> {
        let Some(status) = request("stop").await? else {
            println!("Server is stopped");
            return Ok(());
        };
        println!(
            "Stopping server (PID {}); waiting for the active job to finish",
            status.pid
        );
        loop {
            sleep(Duration::from_millis(100)).await;
            match request("status").await? {
                Some(current) if current.pid == status.pid => (),
                _ => break,
            }
        }
        println!("Server stopped");
        Ok(())
    }

    pub(crate) async fn start(directory: &Path) -> Result<(), Box<dyn Error>> {
        let directory = std::fs::canonicalize(directory)
            .map_err(|e| format!("cannot open working directory {}: {e}", directory.display()))?;
        if let Some(status) = request("status").await? {
            return Err(format!(
                "background server is already running (PID {}, {}); stop it first",
                status.pid,
                status.working_directory.display()
            )
            .into());
        }
        let log_path = directory.join("server.log");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&log_path)?;
        let mut child = Command::new(std::env::current_exe()?)
            .args(["server", "start", "--background-child", "-d"])
            .arg(&directory)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .process_group(0)
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(exit) = child.try_wait()? {
                return Err(format!(
                    "server failed to start ({exit}); see {}",
                    log_path.display()
                )
                .into());
            }
            match request("status").await {
                Ok(Some(status)) if status.pid == child.id() => {
                    describe(&status);
                    println!("Log: {}", log_path.display());
                    return Ok(());
                }
                Ok(_) => (),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
            if Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                return Err(format!("server startup timed out; see {}", log_path.display()).into());
            }
            sleep(Duration::from_millis(50)).await;
        }
    }

    pub(crate) struct Controller {
        _lock: File,
        socket_path: PathBuf,
        working_directory: PathBuf,
    }

    impl Controller {
        pub(crate) fn prepare(working_directory: &Path) -> Result<Self, Box<dyn Error>> {
            let directory = state_directory()?;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&directory)?;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .mode(0o600)
                .open(directory.join("server.lock"))?;
            lock.try_lock()
                .map_err(|e| format!("another background server is starting or running: {e}"))?;
            let socket_path = directory.join("server.sock");
            match std::fs::remove_file(&socket_path) {
                Ok(()) => (),
                Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
            Ok(Self {
                _lock: lock,
                socket_path,
                working_directory: working_directory.to_owned(),
            })
        }

        pub(crate) fn listen(
            &self,
            address: SocketAddr,
            stop: watch::Sender<bool>,
        ) -> io::Result<tokio::task::JoinHandle<io::Result<()>>> {
            let listener = UnixListener::bind(&self.socket_path)?;
            std::fs::set_permissions(&self.socket_path, std::fs::Permissions::from_mode(0o600))?;
            let working_directory = self.working_directory.clone();
            Ok(tokio::spawn(async move {
                loop {
                    let (stream, _) = listener.accept().await?;
                    let stop = stop.clone();
                    let working_directory = working_directory.clone();
                    // One stalled local client must not block other control requests.
                    tokio::spawn(async move {
                        let exchange = async {
                            let mut reader = BufReader::new(stream);
                            let mut command = String::new();
                            reader.read_line(&mut command).await?;
                            match command.trim() {
                                "stop" => {
                                    stop.send_replace(true);
                                }
                                "status" => (),
                                _ => return Ok::<_, io::Error>(()),
                            }
                            let status = Status {
                                pid: std::process::id(),
                                working_directory,
                                address,
                                stopping: *stop.borrow(),
                            };
                            let mut response = serde_json::to_vec(&status)?;
                            response.push(b'\n');
                            reader.get_mut().write_all(&response).await
                        };
                        if let Ok(Err(error)) = timeout(Duration::from_secs(3), exchange).await {
                            eprintln!("server control request failed: {error}");
                        }
                    });
                }
            }))
        }
    }

    impl Drop for Controller {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_file(&self.socket_path)
                && error.kind() != io::ErrorKind::NotFound
            {
                eprintln!(
                    "cannot remove server control socket {}: {error}",
                    self.socket_path.display()
                );
            }
        }
    }
}

#[cfg(unix)]
pub(crate) use unix::{Controller, start, status, stop};

#[cfg(not(unix))]
mod unsupported {
    use super::*;
    pub(crate) struct Controller;
    impl Controller {
        pub(crate) fn prepare(_: &Path) -> Result<Self, Box<dyn Error>> {
            Err("background servers currently require Unix; use --foreground".into())
        }
        pub(crate) fn listen(
            &self,
            _: std::net::SocketAddr,
            _: tokio::sync::watch::Sender<bool>,
        ) -> std::io::Result<tokio::task::JoinHandle<std::io::Result<()>>> {
            Err(std::io::Error::other(
                "background servers currently require Unix",
            ))
        }
    }
    pub(crate) async fn start(_: &Path) -> Result<(), Box<dyn Error>> {
        Err("background servers currently require Unix; use --foreground".into())
    }
    pub(crate) async fn status() -> Result<(), Box<dyn Error>> {
        Err("background servers currently require Unix".into())
    }
    pub(crate) async fn stop() -> Result<(), Box<dyn Error>> {
        Err("background servers currently require Unix".into())
    }
}
#[cfg(not(unix))]
pub(crate) use unsupported::{Controller, start, status, stop};
