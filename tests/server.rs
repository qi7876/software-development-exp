// Exercise the compiled application through HTTP and its durable output files.
// Backup/restore and SIGTERM shutdown currently require Unix.
#![cfg(unix)]

use proptest::{
    collection::{btree_map, vec},
    prelude::*,
    test_runner::{Config, FileFailurePersistence, RngSeed, TestCaseError},
};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
use std::{
    collections::BTreeMap,
    error::Error,
    fs::OpenOptions,
    io::{self, BufRead, BufReader, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::time::{Instant, sleep, timeout};

const SECRET: &str = "integration-test-key";
const LIST_PATH: &str = "/api/backups?repository=repo+with+space";
type TestResult<T = ()> = Result<T, Box<dyn Error>>;
type Tree = BTreeMap<PathBuf, Option<Vec<u8>>>;

fn unfiltered() -> Value {
    json!({
        "relative_path": {"include": [], "exclude": []},
        "uid": {"include": [], "exclude": []},
        "gid": {"include": [], "exclude": []},
        "mode": {"include": [], "exclude": []},
        "atime": {"from": null, "before": null}, "mtime": {"from": null, "before": null}
    })
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!("bak-integration-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("cannot remove {}: {error}", self.0.display());
        }
    }
}

struct TestServer {
    directory: TestDirectory,
    client: Client,
    address: SocketAddr,
    child: Option<Child>,
    output: Option<JoinHandle<io::Result<()>>>,
}

impl TestServer {
    async fn start() -> TestResult<Self> {
        let directory = TestDirectory::new()?;
        for name in ["source", "repo with space", "restored"] {
            std::fs::create_dir(directory.0.join(name))?;
        }
        std::fs::write(
            directory.0.join("config.json"),
            serde_json::to_vec(&json!({
                "listen": "127.0.0.1:0",
                "secret_key": SECRET,
                "web_ui": {"enabled": false, "archive_url": "https://example.com/ui.zip", "directory": "web-ui"},
            }))?,
        )?;
        let mut server = Self {
            directory,
            client: Client::builder()
                .no_proxy()
                .http1_only()
                .pool_max_idle_per_host(0)
                .timeout(Duration::from_secs(2))
                .build()?,
            address: "127.0.0.1:0".parse()?,
            child: None,
            output: None,
        };
        server.spawn()?;
        server.wait_ready().await?;
        Ok(server)
    }

    fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.directory.0.join(relative)
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    fn logs(&self) -> String {
        std::fs::read_to_string(self.path("server.log")).unwrap_or_default()
    }

    fn spawn(&mut self) -> TestResult {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path("server.log"))?;
        let mut stdout_log = log.try_clone()?;
        let child = Command::new(env!("CARGO_BIN_EXE_bak"))
            .arg("-d")
            .arg(&self.directory.0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()?;
        self.child = Some(child);
        let stdout = self
            .child
            .as_mut()
            .ok_or("missing server process")?
            .stdout
            .take()
            .ok_or("missing server stdout")?;
        let (ready, receiver) = mpsc::channel();
        self.output = Some(thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = line?;
                if let Some(address) = line.strip_prefix("Listening on ") {
                    let _ = ready.send(
                        address
                            .parse::<SocketAddr>()
                            .map_err(|error| error.to_string()),
                    );
                }
                writeln!(stdout_log, "{line}")?;
            }
            Ok(())
        }));
        self.address = receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|error| format!("server startup failed: {error}\n{}", self.logs()))??;
        Ok(())
    }

    async fn wait_ready(&mut self) -> TestResult {
        timeout(Duration::from_secs(5), async {
            loop {
                if let Some(status) = self
                    .child
                    .as_mut()
                    .ok_or("missing server process")?
                    .try_wait()?
                {
                    return Err(
                        format!("server exited during startup: {status}\n{}", self.logs()).into(),
                    );
                }
                if let Ok(response) = self
                    .client
                    .get(self.url("/api/status"))
                    .bearer_auth(SECRET)
                    .send()
                    .await
                    && response.status() == StatusCode::OK
                {
                    let body: Value = response.json().await?;
                    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
                    return Ok::<_, Box<dyn Error>>(());
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await??;
        Ok(())
    }

    async fn get(&self, path: &str) -> TestResult<Value> {
        Ok(self
            .client
            .get(self.url(path))
            .bearer_auth(SECRET)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    async fn submit(&self, path: &str, payload: Value) -> TestResult<String> {
        let response = self
            .client
            .post(self.url(path))
            .bearer_auth(SECRET)
            .json(&payload)
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let location = response
            .headers()
            .get("Location")
            .ok_or("missing job location")?
            .to_str()?
            .to_owned();
        let accepted: Value = response.json().await?;
        assert_eq!(accepted["state"], "queued");
        assert!(accepted["result"].is_null());
        assert!(accepted["error"].is_null());
        let id = accepted["job_id"]
            .as_str()
            .ok_or("missing accepted job ID")?
            .to_owned();
        assert_eq!(location, format!("/api/jobs/{id}"));
        Ok(id)
    }

    async fn backup(&self) -> TestResult<String> {
        self.submit(
            "/api/backups",
            json!({"source": "source", "repository": "repo with space", "filter": unfiltered()}),
        )
        .await
    }

    async fn wait_job(&self, id: &str, running: bool) -> TestResult<Value> {
        timeout(Duration::from_secs(5), async {
            loop {
                let job = self.get(&format!("/api/jobs/{id}")).await?;
                let terminal = job["state"] == "succeeded" || job["state"] == "failed";
                if running && job["state"] == "running" || !running && terminal {
                    return Ok::<_, Box<dyn Error>>(job);
                }
                if terminal {
                    return Err(format!("job completed before active-state check: {job}").into());
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await?
    }

    fn signal_stop(&mut self) -> TestResult {
        let child = self.child.as_mut().ok_or("missing server process")?;
        if let Some(status) = child.try_wait()? {
            return Err(format!("server exited before SIGTERM: {status}\n{}", self.logs()).into());
        }
        let result = Command::new("kill")
            .arg("-TERM")
            .arg(child.id().to_string())
            .status()?;
        assert!(result.success(), "could not signal the server");
        Ok(())
    }

    async fn wait_exit(&mut self) -> TestResult<ExitStatus> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self
                .child
                .as_mut()
                .ok_or("missing server process")?
                .try_wait()?
            {
                self.child = None;
                if let Some(output) = self.output.take() {
                    output.join().map_err(|_| "stdout collector panicked")??;
                }
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(format!("server did not exit\n{}", self.logs()).into());
            }
            sleep(Duration::from_millis(5)).await;
        }
    }

    async fn stop(&mut self) -> TestResult {
        self.signal_stop()?;
        let status = self.wait_exit().await?;
        assert!(status.success(), "server failed: {status}\n{}", self.logs());
        Ok(())
    }

    async fn restart(&mut self) -> TestResult {
        self.stop().await?;
        self.spawn()?;
        self.wait_ready().await
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(output) = self.output.take() {
            let _ = output.join();
        }
    }
}

fn read_tree(root: &Path) -> io::Result<Tree> {
    fn walk(root: &Path, current: &Path, entries: &mut Tree) -> io::Result<()> {
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(io::Error::other)?
                .to_owned();
            if entry.file_type()?.is_dir() {
                entries.insert(relative, None);
                walk(root, &path, entries)?;
            } else {
                entries.insert(relative, Some(std::fs::read(&path)?));
            }
        }
        Ok(())
    }
    let mut entries = Tree::new();
    walk(root, root, &mut entries)?;
    Ok(entries)
}

async fn round_trip(files: BTreeMap<String, Vec<u8>>) -> TestResult {
    let mut server = TestServer::start().await?;
    for (relative, contents) in files {
        let path = server.path("source").join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)?;
    }
    std::fs::create_dir_all(server.path("source/empty/nested"))?;
    let original = read_tree(&server.path("source"))?;
    assert_eq!(server.get(LIST_PATH).await?, json!({"backups": []}));
    assert!(!server.path("repo with space/repository.sqlite3").exists());

    let backup_job_id = server.backup().await?;
    let backup_job = server.wait_job(&backup_job_id, false).await?;
    assert_eq!(backup_job["state"], "succeeded", "{backup_job}");
    assert_eq!(backup_job["kind"], "backup");
    assert!(backup_job["error"].is_null());
    let backup_id = backup_job["result"]["backup_id"]
        .as_str()
        .ok_or("missing backup ID")?;
    let payloads = server.path("repo with space/backups").join(backup_id);
    let compressed = read_tree(&payloads)?;
    std::fs::create_dir_all(server.path("repo with space/backups/unpublished"))?;
    let listed = server.get(LIST_PATH).await?;
    let summaries = listed["backups"].as_array().ok_or("missing summaries")?;
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0]["backup_id"], backup_id);
    let created = summaries[0]["created_at"]
        .as_str()
        .ok_or("missing creation timestamp")?;
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(created)?
            .offset()
            .local_minus_utc(),
        0
    );
    assert_eq!(
        listed,
        json!({"backups": [{"backup_id": backup_id, "created_at": created}]})
    );

    server.restart().await?;
    assert_eq!(
        server.get(&format!("/api/jobs/{backup_job_id}")).await?,
        backup_job
    );
    assert_eq!(server.get(LIST_PATH).await?, listed);
    let restore_job_id = server
        .submit(
            "/api/restores",
            json!({
                "repository": "repo with space", "backup_id": backup_id, "destination": "restored",
            }),
        )
        .await?;
    let restore_job = server.wait_job(&restore_job_id, false).await?;
    assert_eq!(restore_job["state"], "succeeded", "{restore_job}");
    assert_eq!(restore_job["kind"], "restore");
    assert_eq!(restore_job["result"], json!({"destination": "restored"}));
    assert!(restore_job["error"].is_null());
    assert_eq!(read_tree(&server.path("restored"))?, original);
    assert_eq!(read_tree(&server.path("source"))?, original);
    assert_eq!(read_tree(&payloads)?, compressed);
    assert_eq!(server.get(LIST_PATH).await?, listed);
    let response = server
        .client
        .post(server.url(&format!("/api/jobs/{restore_job_id}/cancel")))
        .bearer_auth(SECRET)
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    server.stop().await
}

async fn occupied_destination_is_preserved(
    entries: BTreeMap<String, Option<Vec<u8>>>,
) -> TestResult {
    let mut server = TestServer::start().await?;
    std::fs::write(server.path("source/payload"), b"keep source contents")?;
    for (relative, contents) in entries {
        let path = server.path("restored").join(relative);
        match contents {
            Some(contents) => {
                std::fs::create_dir_all(path.parent().ok_or("missing destination parent")?)?;
                std::fs::write(path, contents)?;
            }
            None => std::fs::create_dir_all(path)?,
        }
    }
    let occupied = read_tree(&server.path("restored"))?;
    assert!(!occupied.is_empty());
    let original = read_tree(&server.path("source"))?;
    let backup_job_id = server.backup().await?;
    let backup_job = server.wait_job(&backup_job_id, false).await?;
    assert_eq!(backup_job["state"], "succeeded", "{backup_job}");
    let backup_id = backup_job["result"]["backup_id"]
        .as_str()
        .ok_or("missing backup ID")?;
    let listed = server.get(LIST_PATH).await?;
    let repository = read_tree(&server.path("repo with space"))?;

    let restore_job_id = server
        .submit(
            "/api/restores",
            json!({
                "repository": "repo with space", "backup_id": backup_id, "destination": "restored",
            }),
        )
        .await?;
    let failed = server.wait_job(&restore_job_id, false).await?;
    assert_eq!(failed["state"], "failed", "{failed}");
    assert_eq!(failed["kind"], "restore");
    assert!(failed["result"].is_null());
    assert_eq!(failed["error"]["code"], 400);
    assert_eq!(read_tree(&server.path("restored"))?, occupied);
    assert_eq!(read_tree(&server.path("source"))?, original);
    assert_eq!(read_tree(&server.path("repo with space"))?, repository);
    assert_eq!(server.get(LIST_PATH).await?, listed);

    server.restart().await?;
    assert_eq!(
        server.get(&format!("/api/jobs/{restore_job_id}")).await?,
        failed
    );
    assert_eq!(read_tree(&server.path("restored"))?, occupied);
    assert_eq!(read_tree(&server.path("source"))?, original);
    assert_eq!(server.get(LIST_PATH).await?, listed);
    server.stop().await
}

#[tokio::test]
async fn filters_survive_restart_and_control_backup_and_restore_through_http() -> TestResult {
    let mut server = TestServer::start().await?;
    std::fs::create_dir(server.path("source/docs"))?;
    std::fs::create_dir(server.path("source/blocked"))?;
    std::fs::write(server.path("source/docs/keep.txt"), b"selected")?;
    std::fs::write(server.path("source/docs/drop.tmp"), b"excluded")?;
    std::fs::write(server.path("source/blocked/unvisited.txt"), b"pruned")?;
    std::fs::write(server.path("source/root.bin"), b"not selected")?;
    let mut filter = unfiltered();
    filter["relative_path"] = json!({"include": ["^docs(/|$)"], "exclude": ["\\.tmp$"]});
    let id = server
        .submit(
            "/api/backups",
            json!({
                "source": "source", "repository": "repo with space", "filter": filter
            }),
        )
        .await?;
    let backup = server.wait_job(&id, false).await?;
    assert_eq!(backup["state"], "succeeded");
    let backup_id = backup["result"]["backup_id"]
        .as_str()
        .ok_or("missing backup ID")?;
    let history = server.get("/api/jobs").await?;
    assert_eq!(history["jobs"][0]["request"]["filter"], filter);
    server.restart().await?;
    assert_eq!(server.get("/api/jobs").await?, history);
    let id = server
        .submit(
            "/api/restores",
            json!({
                "repository": "repo with space", "backup_id": backup_id, "destination": "restored"
            }),
        )
        .await?;
    assert_eq!(server.wait_job(&id, false).await?["state"], "succeeded");
    assert_eq!(
        read_tree(&server.path("restored"))?,
        [
            (PathBuf::from("docs"), None),
            (PathBuf::from("docs/keep.txt"), Some(b"selected".to_vec())),
        ]
        .into()
    );
    server.stop().await
}

#[tokio::test]
async fn invalid_filter_jobs_fail_before_creating_repository_contents() -> TestResult {
    let mut server = TestServer::start().await?;
    let mut invalid_regex = unfiltered();
    invalid_regex["relative_path"]["exclude"] = json!(["["]);
    let mut invalid_range = unfiltered();
    invalid_range["mtime"] =
        json!({"from": "2026-10-26T00:00:00Z", "before": "2026-10-25T00:00:00Z"});
    for filter in [invalid_regex, invalid_range] {
        let id = server
            .submit(
                "/api/backups",
                json!({
                    "source": "source", "repository": "repo with space", "filter": filter
                }),
            )
            .await?;
        let failed = server.wait_job(&id, false).await?;
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["error"]["code"], 400);
        assert_eq!(server.get(LIST_PATH).await?, json!({"backups": []}));
        assert!(!server.path("repo with space/backups").exists());
        assert!(!server.path("repo with space/repository.sqlite3").exists());
    }
    server.stop().await
}

#[tokio::test]
async fn job_history_paginates_without_duplicates_and_survives_restart() -> TestResult {
    let mut server = TestServer::start().await?;
    assert_eq!(
        server.get("/api/jobs").await?,
        json!({"jobs": [], "next_cursor": null})
    );
    let mut ids = Vec::new();
    for source in ["missing-first", "missing-second", "missing-third"] {
        let id = server
            .submit(
                "/api/backups",
                json!({"source": source, "repository": "repo with space", "filter": unfiltered()}),
            )
            .await?;
        server.wait_job(&id, false).await?;
        ids.push(id);
    }
    let first = server.get("/api/jobs?limit=2").await?;
    assert_eq!(first["jobs"][0]["job_id"], ids[2]);
    assert_eq!(first["jobs"][1]["job_id"], ids[1]);
    let item = &first["jobs"][0];
    assert_eq!(
        item["request"],
        json!({"kind": "Backup", "source": "missing-third", "repository": "repo with space", "filter": unfiltered()})
    );
    // Sequence defines ordering; sampled wall-clock timestamps need not be monotonic.
    for field in ["queued_at", "started_at", "finished_at"] {
        assert!(item[field].is_i64(), "missing {field}: {item}");
    }
    assert_eq!(item["state"], "failed");
    assert!(
        item["error"]["message"]
            .as_str()
            .ok_or("missing error")?
            .contains("missing-third")
    );
    let cursor = first["next_cursor"].as_str().ok_or("missing cursor")?;
    let new_id = server.backup().await?;
    server.wait_job(&new_id, false).await?;
    let second = server
        .get(&format!("/api/jobs?limit=2&before={cursor}"))
        .await?;
    assert_eq!(second["jobs"].as_array().ok_or("missing jobs")?.len(), 1);
    assert_eq!(second["jobs"][0]["job_id"], ids[0]);
    assert!(second["next_cursor"].is_null());
    let history = server.get("/api/jobs").await?;
    assert_eq!(
        history["jobs"].as_array().ok_or("missing history")?.len(),
        4
    );
    server.restart().await?;
    assert_eq!(server.get("/api/jobs").await?, history);
    server.stop().await
}

#[tokio::test]
async fn startup_authentication_and_failed_job_reporting() -> TestResult {
    let mut server = TestServer::start().await?;
    let response = server.client.get(server.url("/api/status")).send().await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get("WWW-Authenticate")
            .ok_or("missing auth challenge")?,
        "Bearer"
    );
    let body: Value = response.json().await?;
    assert_eq!(body["error"]["code"], 401);
    assert_eq!(server.get(LIST_PATH).await?, json!({"backups": []}));
    assert!(!server.path("repo with space/repository.sqlite3").exists());
    let failed_id = server
        .submit(
            "/api/backups",
            json!({
                "source": "missing-source", "repository": "repo with space", "filter": unfiltered(),
            }),
        )
        .await?;
    let failed = server.wait_job(&failed_id, false).await?;
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["kind"], "backup");
    assert!(failed["result"].is_null());
    assert_eq!(failed["error"]["code"], 500);
    assert!(
        failed["error"]["message"]
            .as_str()
            .ok_or("missing failure message")?
            .contains("missing-source")
    );
    assert_eq!(
        server.get("/api/status").await?["version"],
        env!("CARGO_PKG_VERSION")
    );
    server.stop().await
}

#[tokio::test]
async fn buffer_boundaries_and_unicode_paths_round_trip() -> TestResult {
    let mut files: BTreeMap<_, _> = [0, 1, 131_071, 131_072, 131_073, 300_001]
        .into_iter()
        .map(|size| {
            (
                format!("nested/{size}.bin"),
                (0u8..=255).cycle().take(size).collect(),
            )
        })
        .collect();
    files.insert("目录/hello world.txt".into(), b"Unicode path".to_vec());
    round_trip(files).await
}

proptest! {
    // Process-level cases cost more than component cases; retain reproducibility and shrinking.
    #![proptest_config(Config {
        cases: 8,
        rng_seed: RngSeed::Fixed(0x5E12_2026),
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(concat!(
            env!("CARGO_MANIFEST_DIR"), "/tests/server.proptest-regressions"
        )))),
        ..Config::default()
    })]

    #[test]
    fn generated_files_round_trip_through_http(
        files in btree_map("(root|nested|nested/deeper)/[a-z]{1,6}\\.bin", vec(any::<u8>(), 0..2049), 0..5),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        runtime.block_on(round_trip(files)).map_err(|error| TestCaseError::fail(error.to_string()))?;
    }

    #[test]
    fn generated_occupied_destinations_are_preserved_through_http(
        entries in btree_map(
            "(root|nested|nested/deeper)/entry_[a-z]{1,6}",
            prop::option::of(vec(any::<u8>(), 0..2049)),
            1..5,
        ),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        runtime.block_on(occupied_destination_is_preserved(entries))
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
    }
}

#[tokio::test]
async fn abrupt_exit_fails_the_active_job_and_resumes_queued_work() -> TestResult {
    let mut server = TestServer::start().await?;
    std::fs::write(server.path("source/payload"), b"keep source contents")?;
    let original = read_tree(&server.path("source"))?;
    let completed_id = server.backup().await?;
    let completed = server.wait_job(&completed_id, false).await?;
    assert_eq!(completed["state"], "succeeded", "{completed}");
    let backup_id = completed["result"]["backup_id"]
        .as_str()
        .ok_or("missing backup ID")?;
    let payloads = server.path("repo with space/backups").join(backup_id);
    let compressed = read_tree(&payloads)?;

    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(server.path("repo with space/repository.sqlite3"))
            .create_if_missing(false),
    )
    .await?;
    // Block publication so the process cannot complete the active job before it is killed.
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut connection)
        .await?;
    let active = server.backup().await?;
    server.wait_job(&active, true).await?;
    let queued = server.backup().await?;
    assert_eq!(
        server.get(&format!("/api/jobs/{queued}")).await?["state"],
        "queued"
    );
    server
        .child
        .as_mut()
        .ok_or("missing server process")?
        .kill()?;
    let status = server.wait_exit().await?;
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(9)
    );
    sqlx::query("ROLLBACK").execute(&mut connection).await?;
    connection.close().await?;

    server.spawn()?;
    server.wait_ready().await?;
    let interrupted = server.wait_job(&active, false).await?;
    assert_eq!(interrupted["state"], "failed", "{interrupted}");
    assert_eq!(interrupted["kind"], "backup");
    assert!(interrupted["result"].is_null());
    assert_eq!(interrupted["error"]["code"], 500);
    assert!(
        interrupted["error"]["message"]
            .as_str()
            .ok_or("missing interruption message")?
            .contains("interrupted")
    );
    let resumed = server.wait_job(&queued, false).await?;
    assert_eq!(resumed["state"], "succeeded", "{resumed}");
    assert!(resumed["error"].is_null());
    assert_eq!(
        server.get(&format!("/api/jobs/{completed_id}")).await?,
        completed
    );
    assert_eq!(
        server.get(&format!("/api/jobs/{active}")).await?,
        interrupted
    );
    let listed = server.get(LIST_PATH).await?;
    let summaries = listed["backups"].as_array().ok_or("missing summaries")?;
    assert_eq!(summaries.len(), 2);
    for id in [
        backup_id,
        resumed["result"]["backup_id"]
            .as_str()
            .ok_or("missing resumed backup ID")?,
    ] {
        assert!(summaries.iter().any(|summary| summary["backup_id"] == id));
    }
    assert_eq!(read_tree(&server.path("source"))?, original);
    assert_eq!(read_tree(&payloads)?, compressed);
    server.stop().await
}

#[tokio::test]
async fn http_stays_responsive_and_shutdown_finishes_only_the_active_job() -> TestResult {
    let mut server = TestServer::start().await?;
    std::fs::write(server.path("source/payload"), b"keep source contents")?;
    let seed = server.backup().await?;
    let seeded = server.wait_job(&seed, false).await?;
    assert_eq!(seeded["state"], "succeeded", "{seeded}");
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(server.path("repo with space/repository.sqlite3"))
            .create_if_missing(false),
    )
    .await?;
    // Keep real metadata publication pending, without slowing or replacing application code.
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut connection)
        .await?;
    let active = server.backup().await?;
    server.wait_job(&active, true).await?;
    let queued = server.backup().await?;
    assert_eq!(
        server.get("/api/status").await?["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        server.get(&format!("/api/jobs/{active}")).await?["state"],
        "running"
    );
    assert_eq!(
        server.get(&format!("/api/jobs/{queued}")).await?["state"],
        "queued"
    );
    let admitted = server.backup().await?;
    assert_eq!(
        server.get(&format!("/api/jobs/{admitted}")).await?["state"],
        "queued"
    );

    server.signal_stop()?;
    // Wait for the listener to close before releasing the active job, so shutdown
    // has definitely been requested and the queued jobs cannot be claimed.
    timeout(Duration::from_secs(2), async {
        while tokio::net::TcpStream::connect(server.address).await.is_ok() {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    assert!(
        server
            .child
            .as_mut()
            .ok_or("missing server process")?
            .try_wait()?
            .is_none()
    );
    sqlx::query("COMMIT").execute(&mut connection).await?;
    connection.close().await?;
    let status = server.wait_exit().await?;
    assert!(
        status.success(),
        "server failed: {status}\n{}",
        server.logs()
    );

    let mut jobs = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(server.path("jobs.sqlite3"))
            .create_if_missing(false),
    )
    .await?;
    for (id, expected) in [
        (&active, "succeeded"),
        (&queued, "queued"),
        (&admitted, "queued"),
    ] {
        let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE job_id = ?")
            .bind(id)
            .fetch_one(&mut jobs)
            .await?;
        assert_eq!(state, expected);
    }
    jobs.close().await?;
    server.spawn()?;
    server.wait_ready().await?;
    for id in [&active, &queued, &admitted] {
        let job = server.wait_job(id, false).await?;
        assert_eq!(job["state"], "succeeded", "{job}");
        assert!(job["error"].is_null());
    }
    assert_eq!(
        std::fs::read(server.path("source/payload"))?,
        b"keep source contents"
    );
    server.stop().await
}
