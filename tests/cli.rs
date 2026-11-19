#![cfg(unix)]

use std::{
    error::Error,
    path::{Path, PathBuf},
    process::{Command, Output},
};

type TestResult = Result<(), Box<dyn Error>>;

struct BackgroundServer(PathBuf);

impl BackgroundServer {
    fn new() -> Result<Self, Box<dyn Error>> {
        // Keep the Unix socket path within macOS's length limit.
        let path = PathBuf::from("/tmp").join(format!("bak-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn command(&self, args: &[&str]) -> Result<Output, Box<dyn Error>> {
        Ok(Command::new(env!("CARGO_BIN_EXE_bak"))
            .args(args)
            .env("XDG_STATE_HOME", self.0.join("state"))
            .output()?)
    }

    fn config(&self, directory: &Path) -> TestResult {
        std::fs::create_dir_all(directory)?;
        std::fs::write(
            directory.join("config.json"),
            r#"{"listen":"127.0.0.1:0","secret_key":"test"}"#,
        )?;
        Ok(())
    }

    fn start(&self, directory: &Path) -> Result<Output, Box<dyn Error>> {
        self.command(&[
            "server",
            "start",
            "-d",
            directory.to_str().ok_or("invalid test directory")?,
        ])
    }
}

impl Drop for BackgroundServer {
    fn drop(&mut self) {
        let _ = self.command(&["server", "stop"]);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn command_structure_and_help() -> TestResult {
    let fixture = BackgroundServer::new()?;
    for args in [
        vec!["help"],
        vec!["--help"],
        vec!["-h"],
        vec!["help", "server"],
        vec!["help", "server", "start"],
        vec!["help", "completions"],
        vec!["completions", "bash", "--help"],
        vec!["completions", "fish", "--help"],
        vec!["completions", "zsh", "--help"],
        vec!["version"],
        vec!["server", "start", "--help"],
        vec!["keygen", "--help"],
    ] {
        success(&fixture.command(&args)?);
    }
    for args in [
        vec![],
        vec!["--version"],
        vec!["-d", "."],
        vec!["server", "start"],
        vec!["server", "-d", "."],
        vec!["completions"],
        vec!["completions", "powershell"],
    ] {
        assert!(!fixture.command(&args)?.status.success());
    }
    let help = fixture.command(&["--help"])?;
    assert!(String::from_utf8_lossy(&help.stdout).contains("--help"));
    Ok(())
}

#[test]
fn completion_scripts_include_public_commands_and_options() -> TestResult {
    let fixture = BackgroundServer::new()?;
    for (shell, registration) in [
        ("bash", "complete -F _bak"),
        ("fish", "complete -c bak"),
        ("zsh", "#compdef bak"),
    ] {
        let output = fixture.command(&["completions", shell])?;
        success(&output);
        assert!(output.stderr.is_empty());

        let script = std::str::from_utf8(&output.stdout)?;
        assert!(script.contains(registration), "invalid {shell} script");
        for candidate in [
            "completions",
            "bash",
            "fish",
            "zsh",
            "server",
            "start",
            "status",
            "stop",
            "keygen",
            "version",
            "help",
            "working-directory",
            "foreground",
            "output",
        ] {
            assert!(
                script.contains(candidate),
                "missing {shell} completion for {candidate}"
            );
        }
        assert!(!script.contains("background-child"));
    }
    Ok(())
}

#[test]
fn background_lifecycle_duplicate_start_and_stale_socket_recovery() -> TestResult {
    let fixture = BackgroundServer::new()?;
    let directory = fixture.0.join("work with spaces");
    fixture.config(&directory)?;
    let stopped = fixture.command(&["server", "status"])?;
    success(&stopped);
    assert!(String::from_utf8_lossy(&stopped.stdout).contains("stopped"));
    success(&fixture.command(&["server", "stop"])?);

    let start = fixture.start(&directory)?;
    success(&start);
    let text = String::from_utf8_lossy(&start.stdout);
    assert!(text.contains("Running (PID "));
    assert!(text.contains("work with spaces"));
    let address = text
        .lines()
        .find_map(|line| line.strip_prefix("Listening on "))
        .ok_or("missing address")?;
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let response = reqwest::Client::new()
            .get(format!("http://{address}/api/status"))
            .bearer_auth("test")
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        Ok::<_, Box<dyn Error>>(())
    })?;
    success(&fixture.command(&["server", "status"])?);
    assert!(!fixture.start(&directory)?.status.success());
    let other = fixture.0.join("other");
    fixture.config(&other)?;
    assert!(!fixture.start(&other)?.status.success());
    let foreground = fixture.command(&[
        "server",
        "start",
        "--foreground",
        "-d",
        directory.to_str().ok_or("invalid path")?,
    ])?;
    assert!(!foreground.status.success());

    // A killed server leaves a socket, but cannot leave a live file lock.
    let pid = text
        .split("PID ")
        .nth(1)
        .and_then(|part| part.split(')').next())
        .ok_or("missing PID")?;
    assert!(
        Command::new("kill")
            .args(["-KILL", pid])
            .status()?
            .success()
    );
    for _ in 0..100 {
        let status = fixture.command(&["server", "status"])?;
        if String::from_utf8_lossy(&status.stdout).contains("stopped") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    success(&fixture.start(&directory)?);
    success(&fixture.command(&["server", "stop"])?);
    assert!(
        String::from_utf8_lossy(&fixture.command(&["server", "status"])?.stdout)
            .contains("stopped")
    );
    success(&fixture.start(&other)?);
    success(&fixture.command(&["server", "stop"])?);
    Ok(())
}

#[test]
fn startup_failure_is_reported_and_releases_locks() -> TestResult {
    let fixture = BackgroundServer::new()?;
    let directory = fixture.0.join("work");
    std::fs::create_dir(&directory)?;
    let failed = fixture.start(&directory)?;
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("server.log"));
    assert!(std::fs::read_to_string(directory.join("server.log"))?.contains("cannot read config"));
    fixture.config(&directory)?;
    let occupied = std::net::TcpListener::bind("127.0.0.1:0")?;
    std::fs::write(
        directory.join("config.json"),
        serde_json::to_vec(&serde_json::json!({
            "listen": occupied.local_addr()?, "secret_key": "test"
        }))?,
    )?;
    assert!(!fixture.start(&directory)?.status.success());
    assert!(std::fs::read_to_string(directory.join("server.log"))?.contains("cannot listen"));
    drop(occupied);
    success(&fixture.start(&directory)?);
    success(&fixture.command(&["server", "stop"])?);
    Ok(())
}

#[tokio::test]
async fn stop_waits_for_active_backup_and_retains_queued_work() -> TestResult {
    use serde_json::{Value, json};
    use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
    use std::time::Duration;
    use tokio::time::{sleep, timeout};

    let fixture = BackgroundServer::new()?;
    let directory = fixture.0.join("work");
    fixture.config(&directory)?;
    std::fs::create_dir(directory.join("source"))?;
    std::fs::create_dir(directory.join("repo"))?;
    std::fs::write(directory.join("source/file"), "backup contents")?;
    let mut workload = vec![0; 16 * 1024 * 1024];
    getrandom::fill(&mut workload)?;
    std::fs::write(directory.join("source/active-workload"), workload)?;
    let start = fixture.start(&directory)?;
    success(&start);
    let text = String::from_utf8_lossy(&start.stdout);
    let address = text
        .lines()
        .find_map(|line| line.strip_prefix("Listening on "))
        .ok_or("missing address")?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?;
    let backup = || async {
        let job: Value = client
            .post(format!("http://{address}/api/backups"))
            .bearer_auth("test")
            .json(&json!({
                "source": "source", "repository": "repo", "filter": {
                    "relative_path": {"include": [], "exclude": []}, "uid": {"include": [], "exclude": []},
                "gid": {"include": [], "exclude": []}, "mode": {"include": [], "exclude": []}, "atime": {}, "mtime": {}
                }
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok::<_, Box<dyn Error>>(job["job_id"].as_str().ok_or("missing job ID")?.to_owned())
    };
    let wait_state = |id: String, expected: &'static str| {
        let client = client.clone();
        async move {
            timeout(Duration::from_secs(3), async {
                loop {
                    let job: Value = client
                        .get(format!("http://{address}/api/jobs/{id}"))
                        .bearer_auth("test")
                        .send()
                        .await?
                        .error_for_status()?
                        .json()
                        .await?;
                    if job["state"] == expected {
                        return Ok::<_, Box<dyn Error>>(());
                    }
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await?
        }
    };
    wait_state(backup().await?, "succeeded").await?;
    let active = backup().await?;
    wait_state(active.clone(), "running").await?;
    let queued = backup().await?;
    let mut repository = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(directory.join("jobs.sqlite3")),
    )
    .await?;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut repository)
        .await?;
    let mut stop = Command::new(env!("CARGO_BIN_EXE_bak"))
        .args(["server", "stop"])
        .env("XDG_STATE_HOME", fixture.0.join("state"))
        .stdout(std::process::Stdio::null())
        .spawn()?;
    timeout(Duration::from_secs(2), async {
        loop {
            let status = fixture.command(&["server", "status"])?;
            if String::from_utf8_lossy(&status.stdout).starts_with("Stopping") {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, Box<dyn Error>>(())
    })
    .await??;
    assert!(stop.try_wait()?.is_none());
    sqlx::query("COMMIT").execute(&mut repository).await?;
    repository.close().await?;
    timeout(Duration::from_secs(3), async {
        loop {
            if let Some(exit) = stop.try_wait()? {
                assert!(exit.success());
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, Box<dyn Error>>(())
    })
    .await??;
    let mut jobs = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(directory.join("jobs.sqlite3")),
    )
    .await?;
    for (id, expected) in [(&active, "succeeded"), (&queued, "queued")] {
        let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE job_id = ?")
            .bind(id)
            .fetch_one(&mut jobs)
            .await?;
        assert_eq!(state, expected);
    }
    jobs.close().await?;
    Ok(())
}
