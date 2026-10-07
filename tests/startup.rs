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
        let path = std::env::temp_dir().join(format!(
            "data-backup-test-{}-{}.json",
            std::process::id(),
            NEXT_CONFIG.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, contents).expect("write test config");
        Self(path)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_data-backup"));
        command.env("DATA_BACKUP_CONFIG", &self.0);
        command
    }
}

impl Drop for TestConfig {
    fn drop(&mut self) {
        fs::remove_file(&self.0).expect("remove test config");
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
    let config = TestConfig::new(r#"{"listen":"127.0.0.1:0","secret_key":"integration-secret"}"#);
    let mut server = Server(
        config
            .command()
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
    assert!(String::from_utf8_lossy(&output.stderr).contains("accepts no arguments"));
}
