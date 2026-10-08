#![allow(clippy::expect_used)]

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static NEXT_CONFIG: AtomicU64 = AtomicU64::new(0);

struct TestConfig(PathBuf);

impl TestConfig {
    fn new(contents: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "data-backup test-{}-{}",
            std::process::id(),
            NEXT_CONFIG.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).expect("create working directory");
        let path = directory.join("config.json");
        fs::write(&path, contents).expect("write test config");
        Self(path)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_data-backup"));
        command.arg("--working-directory").arg(self.directory());
        command
    }

    fn directory(&self) -> &std::path::Path {
        self.0
            .parent()
            .expect("configuration has a parent directory")
    }
}

impl Drop for TestConfig {
    fn drop(&mut self) {
        fs::remove_dir_all(self.directory()).expect("remove working directory");
    }
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        // Cleanup also runs if an assertion fails.
        self.0.kill().expect("stop test server");
        self.0.wait().expect("reap test server");
    }
}

#[test]
fn binary_starts_http_from_configuration_without_subcommands() {
    for flag in [
        None,
        Some("-d"),
        Some("--working-directory"),
        Some("--working-directory="),
    ] {
        let config =
            TestConfig::new(r#"{"listen":"127.0.0.1:0","secret_key":"integration-secret"}"#);
        let mut command = Command::new(env!("CARGO_BIN_EXE_data-backup"));
        if let Some(flag) = flag {
            // Resolve a relative directory containing spaces from the launch directory.
            command.current_dir(
                config
                    .directory()
                    .parent()
                    .expect("working directory parent"),
            );
            let relative = config
                .directory()
                .file_name()
                .expect("relative directory name");
            if flag.ends_with('=') {
                command.arg(format!(
                    "{flag}{}",
                    relative.to_str().expect("UTF-8 directory name")
                ));
            } else {
                command.arg(flag).arg(relative);
            }
        } else {
            command.current_dir(config.directory());
        }
        let mut server = Server(
            command
                .stderr(Stdio::piped())
                .spawn()
                .expect("start binary"),
        );
        let stderr = server.0.stderr.take().expect("capture startup log");
        let (sender, receiver) = std::sync::mpsc::channel();
        let log_reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read startup log");
            sender.send(line).expect("deliver startup log");
            let mut rest = String::new();
            reader
                .read_to_string(&mut rest)
                .expect("read remaining logs");
            rest
        });
        let line = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("server must start promptly");
        let address = line
            .split("address=")
            .nth(1)
            .expect("log actual bound address")
            .trim()
            .parse::<std::net::SocketAddr>()
            .expect("valid bound address");
        assert!(!line.contains("integration-secret"));

        for (path, authorization, expected_status) in [
            ("/", "", "HTTP/1.1 200"),
            ("/api/status", "", "HTTP/1.1 401"),
            (
                "/api/status",
                "Authorization: Bearer integration-secret\r\n",
                "HTTP/1.1 200",
            ),
        ] {
            let mut stream = TcpStream::connect(address).expect("connect to configured server");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set read deadline");
            write!(
                stream,
                "GET {path} HTTP/1.1\r\nHost: {address}\r\n{authorization}Connection: close\r\n\r\n"
            )
            .expect("send HTTP request");
            let mut response = String::new();
            stream
                .read_to_string(&mut response)
                .expect("read HTTP response");
            assert!(response.starts_with(expected_status), "{response}");
            if !authorization.is_empty() {
                assert!(response.contains("\"backup_available\":false"));
            }
        }
        let signal = Command::new("kill")
            .args(["-TERM", &server.0.id().to_string()])
            .status()
            .expect("send shutdown signal");
        assert!(signal.success());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = server.0.try_wait().expect("check shutdown") {
                assert!(status.success());
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "server must shut down promptly"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(server);
        let remaining_logs = log_reader.join().expect("log reader must finish");
        assert!(remaining_logs.contains("shutting down"));
        assert!(!remaining_logs.contains("integration-secret"));
    }
}

#[test]
fn invalid_configuration_fails_without_disclosing_secrets() {
    for contents in [
        r#"{"listen":"bad","secret_key":"do-not-log-this"}"#,
        r#"{"listen":"127.0.0.1:0","secret_key":""}"#,
        r#"{"listen":"127.0.0.1:0","secret_key":"do-not-log-this","unknown":true}"#,
        r#"{"listen":"127.0.0.1:0","secret_key":"do-not-log-this", broken}"#,
    ] {
        let config = TestConfig::new(contents);
        let output = config.command().output().expect("start binary");
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("server failed"));
        assert!(!stderr.contains("do-not-log-this"));
    }
}

#[test]
fn removed_cli_subcommands_fail() {
    let output = Command::new(env!("CARGO_BIN_EXE_data-backup"))
        .arg("check")
        .output()
        .expect("start binary");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown argument: check"));
}

#[test]
fn missing_configuration_fails_with_the_path() {
    let config = TestConfig::new("{}");
    let mut command = config.command();
    let path = config.0.clone();
    fs::remove_file(&config.0).expect("remove configuration but keep working directory");
    let output = command.output().expect("start binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot read configuration"));
    assert!(stderr.contains(path.to_str().expect("UTF-8 test path")));
}

#[test]
fn occupied_address_fails_with_the_address() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve address");
    let address = listener.local_addr().expect("reserved address");
    let config = TestConfig::new(&format!(
        r#"{{"listen":"{address}","secret_key":"do-not-log-this"}}"#
    ));
    let output = config.command().output().expect("start binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(&format!("cannot listen on {address}")));
    assert!(!stderr.contains("do-not-log-this"));
}

#[test]
fn help_and_version_exit_without_using_the_directory_or_configuration() {
    let config = TestConfig::new("invalid configuration");
    let missing = config.directory().join("missing");
    for flag in ["--version", "-v", "--help", "-h"] {
        let output = Command::new(env!("CARGO_BIN_EXE_data-backup"))
            .args(["-d", missing.to_str().expect("UTF-8 test path"), flag])
            .output()
            .expect("start binary");
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8(output.stdout).expect("UTF-8 help and version");
        assert!(stdout.starts_with(&format!("data-backup {}", env!("CARGO_PKG_VERSION"))));
        if flag == "--help" || flag == "-h" {
            for text in [
                "--version",
                "--help",
                "--working-directory",
                "config.json",
                "listen",
                "secret_key",
            ] {
                assert!(stdout.contains(text), "help must document {text}");
            }
        } else {
            assert_eq!(
                stdout,
                format!("data-backup {}\n", env!("CARGO_PKG_VERSION"))
            );
        }
    }
}

#[test]
fn invalid_arguments_fail_with_usage_guidance() {
    for arguments in [
        vec!["--unknown"],
        vec!["-d"],
        vec!["--working-directory"],
        vec!["--working-directory="],
        vec!["-d", "--version"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_data-backup"))
            .args(arguments)
            .output()
            .expect("start binary");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("--help"));
    }
}

#[test]
fn invalid_working_directory_fails_with_its_path() {
    let config = TestConfig::new("{}");
    for path in [config.directory().join("missing"), config.0.clone()] {
        let output = Command::new(env!("CARGO_BIN_EXE_data-backup"))
            .arg("-d")
            .arg(&path)
            .output()
            .expect("start binary");
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("cannot use working directory"));
        assert!(stderr.contains(path.to_str().expect("UTF-8 path")));
    }
}
