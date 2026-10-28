mod backup;
mod content;
pub(crate) mod crypto;
pub(crate) use content::BackupOptions;
mod filesystem;
mod filter;
mod repository;
mod restore;
mod storage;

pub(crate) use filter::Filter;
use http::StatusCode;
use serde_json::{Value, json};
use sqlx::{Row, SqlitePool, sqlite::SqliteRow};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Notify, watch};
use uuid::Uuid;

const JOBS_SCHEMA: storage::Schema = storage::Schema {
    version: 2,
    sql: "
CREATE TABLE jobs (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL UNIQUE,
    request TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'succeeded', 'failed')),
    result TEXT,
    error_code INTEGER,
    error_message TEXT,
    queued_at INTEGER NOT NULL,
    started_at INTEGER,
    finished_at INTEGER,
    CHECK (
        (state IN ('queued', 'running') AND result IS NULL AND error_code IS NULL AND error_message IS NULL AND finished_at IS NULL) OR
        (state = 'succeeded' AND result IS NOT NULL AND error_code IS NULL AND error_message IS NULL AND finished_at IS NOT NULL) OR
        (state = 'failed' AND result IS NULL AND error_code IS NOT NULL AND error_code BETWEEN 100 AND 999 AND error_message IS NOT NULL AND finished_at IS NOT NULL)
    )
);
CREATE INDEX queued_jobs ON jobs(sequence) WHERE state = 'queued';
CREATE UNIQUE INDEX one_running_job ON jobs(state) WHERE state = 'running';
",
};

#[derive(Clone)]
pub(crate) struct JobManager {
    pool: SqlitePool,
    working_directory: PathBuf,
    wake: Arc<Notify>,
    pub(crate) keys: Arc<crypto::Keyring>,
}

impl JobManager {
    pub(crate) async fn open(
        working_directory: PathBuf,
        keys: crypto::Keyring,
    ) -> Result<Self, JobError> {
        let pool =
            storage::open(&working_directory.join("jobs.sqlite3"), JOBS_SCHEMA, true).await?;
        // A running operation may have performed external effects. Never replay it.
        sqlx::query("UPDATE jobs SET state = 'failed', error_code = 500, error_message = 'operation interrupted by service exit', finished_at = ? WHERE state = 'running'")
            .bind(timestamp()?).execute(&pool).await
            .map_err(|error| JobError::internal(format!("cannot recover interrupted jobs: {error}")))?;
        Ok(Self {
            pool,
            working_directory,
            wake: Arc::new(Notify::new()),
            keys: Arc::new(keys),
        })
    }

    pub(crate) async fn submit(&self, request: JobRequest) -> Result<JobInfo, JobError> {
        let job = JobInfo {
            job_id: Uuid::new_v4().to_string(),
            request,
            state: JobState::Queued,
        };
        let request = serde_json::to_string(&job.request).map_err(|error| {
            JobError::internal(format!("cannot serialize job {}: {error}", job.job_id))
        })?;
        sqlx::query(
            "INSERT INTO jobs (job_id, request, state, queued_at) VALUES (?, ?, 'queued', ?)",
        )
        .bind(&job.job_id)
        .bind(request)
        .bind(timestamp()?)
        .execute(&self.pool)
        .await
        .map_err(|error| JobError::internal(format!("cannot queue job {}: {error}", job.job_id)))?;
        self.wake.notify_one();
        Ok(job)
    }

    pub(crate) async fn get_job(&self, job_id: &str) -> Result<Option<JobInfo>, JobError> {
        let row = sqlx::query("SELECT * FROM jobs WHERE job_id = ?")
            .bind(job_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| JobError::internal(format!("cannot read job {job_id}: {error}")))?;
        row.map(JobInfo::from_row).transpose()
    }

    pub(crate) async fn list_backups(
        &self,
        repository: &Path,
    ) -> Result<Vec<repository::RepoBackupInfo>, JobError> {
        let repository = self.working_directory.join(repository);
        tokio::task::spawn_blocking(move || repository::list(&repository))
            .await
            .map_err(|error| JobError::internal(format!("backup listing task failed: {error}")))?
    }

    pub(crate) async fn list_jobs(
        &self,
        limit: u32,
        before: Option<i64>,
    ) -> Result<Value, JobError> {
        let mut rows =
            sqlx::query("SELECT * FROM jobs WHERE sequence < ? ORDER BY sequence DESC LIMIT ?")
                .bind(before.unwrap_or(i64::MAX))
                .bind(i64::from(limit) + 1)
                .fetch_all(&self.pool)
                .await
                .map_err(|error| JobError::internal(format!("cannot list jobs: {error}")))?;
        let has_more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let mut entries = Vec::with_capacity(rows.len());
        let mut cursor = None;
        for row in rows {
            cursor = Some(row.try_get::<i64, _>("sequence")?);
            let queued_at = row.try_get::<i64, _>("queued_at")?;
            let started_at = row.try_get::<Option<i64>, _>("started_at")?;
            let finished_at = row.try_get::<Option<i64>, _>("finished_at")?;
            let job = JobInfo::from_row(row)?;
            let mut entry = job.to_json()?;
            entry["request"] = serde_json::to_value(&job.request)?;
            entry["queued_at"] = json!(queued_at);
            entry["started_at"] = json!(started_at);
            entry["finished_at"] = json!(finished_at);
            entries.push(entry);
        }
        // Strings avoid losing SQLite integer precision in browser clients.
        let next_cursor = if has_more {
            cursor.map(|value| value.to_string())
        } else {
            None
        };
        Ok(json!({"jobs": entries, "next_cursor": next_cursor}))
    }

    async fn claim(&self) -> Result<Option<JobInfo>, JobError> {
        let row = sqlx::query("UPDATE jobs SET state = 'running', started_at = ? WHERE sequence = (SELECT sequence FROM jobs WHERE state = 'queued' ORDER BY sequence LIMIT 1) AND state = 'queued' RETURNING *")
            .bind(timestamp()?).fetch_optional(&self.pool).await
            .map_err(|error| JobError::internal(format!("cannot claim queued job: {error}")))?;
        row.map(JobInfo::from_row).transpose()
    }

    async fn finish(
        &self,
        job_id: &str,
        result: Result<JobResult, JobError>,
    ) -> Result<(), JobError> {
        let (state, result, error_code, error_message) = match result {
            Ok(result) => (
                "succeeded",
                Some(serde_json::to_string(&result).map_err(|error| {
                    JobError::internal(format!("cannot serialize job {job_id} result: {error}"))
                })?),
                None,
                None,
            ),
            Err(error) => {
                eprintln!("job {job_id} failed: {}", error.message);
                (
                    "failed",
                    None,
                    Some(i64::from(error.code.as_u16())),
                    Some(error.message),
                )
            }
        };
        let updated = sqlx::query("UPDATE jobs SET state = ?, result = ?, error_code = ?, error_message = ?, finished_at = ? WHERE job_id = ? AND state = 'running'")
            .bind(state).bind(result).bind(error_code).bind(error_message).bind(timestamp()?).bind(job_id)
            .execute(&self.pool).await
            .map_err(|error| JobError::internal(format!("cannot persist outcome for job {job_id}: {error}")))?;
        if updated.rows_affected() != 1 {
            return Err(JobError::internal(format!(
                "cannot finish job {job_id}: job is missing or not running"
            )));
        }
        Ok(())
    }

    pub(crate) async fn run_worker(&self, stop: watch::Receiver<bool>) -> Result<(), JobError> {
        let keys = self.keys.clone();
        self.worker(stop, move |request, directory| {
            execute(request, directory, &keys)
        })
        .await
    }

    async fn worker<F>(&self, mut stop: watch::Receiver<bool>, execute: F) -> Result<(), JobError>
    where
        F: Fn(JobRequest, PathBuf) -> Result<JobResult, JobError> + Send + Sync + 'static,
    {
        let execute = Arc::new(execute);
        loop {
            // Register before checking the durable queue to avoid a missed wakeup.
            let wake = self.wake.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            if *stop.borrow() {
                return Ok(());
            }
            if let Some(job) = self.claim().await? {
                let execute = execute.clone();
                let directory = self.working_directory.clone();
                let result = tokio::task::spawn_blocking(move || execute(job.request, directory))
                    .await
                    .unwrap_or_else(|error| {
                        Err(JobError::internal(format!(
                            "operation task failed: {error}"
                        )))
                    });
                self.finish(&job.job_id, result).await?;
                continue;
            }
            tokio::select! {
                () = &mut wake => {},
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() { return Ok(()); }
                }
            }
        }
    }

    pub(crate) async fn close(&self) {
        self.pool.close().await;
    }
}

fn execute(
    request: JobRequest,
    directory: PathBuf,
    keys: &crypto::Keyring,
) -> Result<JobResult, JobError> {
    match request {
        JobRequest::Backup {
            source,
            repository,
            filter,
            options,
        } => {
            let backup_id = Uuid::new_v4().to_string();
            backup::run_with_options(
                &directory.join(source),
                &directory.join(repository),
                &filter,
                &backup_id,
                &options,
                keys,
            )?;
            Ok(JobResult::Backup { backup_id })
        }
        JobRequest::Restore {
            repository,
            backup_id,
            destination,
        } => {
            restore::run_with_keys(
                &directory.join(repository),
                &backup_id,
                &directory.join(&destination),
                keys,
            )?;
            Ok(JobResult::Restore { destination })
        }
    }
}

fn timestamp() -> Result<i64, JobError> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| JobError::internal(format!("cannot timestamp job: {error}")))?
        .as_millis();
    i64::try_from(milliseconds)
        .map_err(|error| JobError::internal(format!("job timestamp is out of range: {error}")))
}

#[derive(Clone, Debug)]
pub(crate) struct JobInfo {
    pub(crate) job_id: String,
    pub(crate) request: JobRequest,
    pub(crate) state: JobState,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub(crate) enum JobRequest {
    Backup {
        source: PathBuf,
        repository: PathBuf,
        filter: Box<Filter>,
        #[serde(flatten)]
        options: BackupOptions,
    },
    Restore {
        repository: PathBuf,
        backup_id: String,
        destination: PathBuf,
    },
}

#[derive(Clone, Debug)]
pub(crate) enum JobState {
    Queued,
    Running,
    Succeeded(JobResult),
    Failed(JobError),
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum JobResult {
    Backup { backup_id: String },
    Restore { destination: PathBuf },
}

#[derive(Clone, Debug)]
pub(crate) struct JobError {
    pub(crate) code: StatusCode,
    pub(crate) message: String,
}

impl JobError {
    pub(crate) fn new(code: StatusCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

impl fmt::Display for JobError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for JobError {}
impl From<sqlx::Error> for JobError {
    fn from(error: sqlx::Error) -> Self {
        Self::internal(format!("database operation failed: {error}"))
    }
}
impl From<serde_json::Error> for JobError {
    fn from(error: serde_json::Error) -> Self {
        Self::internal(format!("cannot encode or decode stored metadata: {error}"))
    }
}

impl JobInfo {
    fn from_row(row: SqliteRow) -> Result<Self, JobError> {
        let job_id: String = row.try_get("job_id")?;
        let decode = || -> Result<Self, JobError> {
            let request = serde_json::from_str(&row.try_get::<String, _>("request")?)?;
            let state = match row.try_get::<String, _>("state")?.as_str() {
                "queued" => JobState::Queued,
                "running" => JobState::Running,
                "succeeded" => {
                    JobState::Succeeded(serde_json::from_str(&row.try_get::<String, _>("result")?)?)
                }
                "failed" => {
                    let code = row.try_get::<i64, _>("error_code")?;
                    let code = u16::try_from(code)
                        .ok()
                        .and_then(|code| StatusCode::from_u16(code).ok())
                        .ok_or_else(|| JobError::internal("invalid stored error status"))?;
                    JobState::Failed(JobError::new(
                        code,
                        row.try_get::<String, _>("error_message")?,
                    ))
                }
                state => {
                    return Err(JobError::internal(format!(
                        "invalid stored job state {state}"
                    )));
                }
            };
            Ok(Self {
                job_id: job_id.clone(),
                request,
                state,
            })
        };
        decode().map_err(|error| JobError::internal(format!("cannot decode job {job_id}: {error}")))
    }

    pub(crate) fn kind_name(&self) -> &'static str {
        match self.request {
            JobRequest::Backup { .. } => "backup",
            JobRequest::Restore { .. } => "restore",
        }
    }

    pub(crate) fn state_name(&self) -> &'static str {
        match self.state {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Succeeded(_) => "succeeded",
            JobState::Failed(_) => "failed",
        }
    }

    pub(crate) fn to_json(&self) -> Result<Value, JobError> {
        let (result, error) = match &self.state {
            JobState::Succeeded(JobResult::Backup { backup_id }) => {
                (json!({"backup_id": backup_id}), Value::Null)
            }
            JobState::Succeeded(JobResult::Restore { destination }) => (
                json!({"destination": serde_json::to_value(destination)?}),
                Value::Null,
            ),
            JobState::Failed(error) => (
                Value::Null,
                json!({"code": error.code.as_u16(), "message": error.message}),
            ),
            JobState::Queued | JobState::Running => (Value::Null, Value::Null),
        };
        Ok(
            json!({"job_id": self.job_id, "kind": self.kind_name(), "state": self.state_name(), "result": result, "error": error}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDirectory;
    use std::{error::Error, time::Duration};

    fn backup_request(source: &str) -> Result<JobRequest, serde_json::Error> {
        Ok(JobRequest::Backup {
            source: source.into(),
            repository: "repository".into(),
            filter: Box::new(crate::test_support::unfiltered()?),
            options: BackupOptions::default(),
        })
    }

    #[tokio::test]
    async fn jobs_survive_restart_and_only_running_jobs_fail_recovery() -> Result<(), Box<dyn Error>>
    {
        let directory = TestDirectory::new()?;
        let jobs =
            JobManager::open(directory.path().to_owned(), crypto::Keyring::default()).await?;
        assert!(jobs.get_job("unknown").await?.is_none());
        let first = jobs.submit(backup_request("one")?).await?;
        let second = jobs.submit(backup_request("two")?).await?;
        let third = jobs
            .submit(JobRequest::Restore {
                repository: "repository".into(),
                backup_id: "backup-001".into(),
                destination: "restored".into(),
            })
            .await?;
        assert_eq!(
            jobs.claim().await?.ok_or("missing queued job")?.job_id,
            first.job_id
        );
        jobs.finish(
            &first.job_id,
            Ok(JobResult::Backup {
                backup_id: "backup-001".into(),
            }),
        )
        .await?;
        assert_eq!(
            jobs.claim().await?.ok_or("missing second job")?.job_id,
            second.job_id
        );
        jobs.close().await;
        let jobs =
            JobManager::open(directory.path().to_owned(), crypto::Keyring::default()).await?;
        assert!(matches!(
            jobs.get_job(&first.job_id)
                .await?
                .ok_or("lost completed history")?
                .state,
            JobState::Succeeded(_)
        ));
        let interrupted = jobs
            .get_job(&second.job_id)
            .await?
            .ok_or("lost interrupted job")?;
        assert!(
            matches!(interrupted.state, JobState::Failed(ref error) if error.code == StatusCode::INTERNAL_SERVER_ERROR && error.message.contains("interrupted"))
        );
        assert!(matches!(
            jobs.get_job(&third.job_id)
                .await?
                .ok_or("lost queued job")?
                .state,
            JobState::Queued
        ));
        let resumed = jobs.claim().await?.ok_or("missing queued restore")?;
        assert_eq!(resumed.job_id, third.job_id);
        jobs.finish(
            &resumed.job_id,
            execute(
                resumed.request,
                directory.path().to_owned(),
                &crypto::Keyring::default(),
            ),
        )
        .await?;
        assert!(jobs.claim().await?.is_none());
        jobs.close().await;
        let jobs =
            JobManager::open(directory.path().to_owned(), crypto::Keyring::default()).await?;
        let failed = jobs
            .get_job(&third.job_id)
            .await?
            .ok_or("lost failed result")?
            .to_json()?;
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["error"]["code"], 500);
        assert!(failed["result"].is_null());
        assert!(
            failed["error"]["message"]
                .as_str()
                .ok_or("missing failure message")?
                .contains("repository")
        );
        jobs.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn worker_serializes_operations_and_finishes_active_job_on_shutdown()
    -> Result<(), Box<dyn Error>> {
        tokio::time::timeout(Duration::from_secs(10), async {
            let directory = TestDirectory::new()?;
            let jobs = JobManager::open(directory.path().to_owned(), crypto::Keyring::default()).await?;
            let first = jobs.submit(backup_request("first")?).await?;
            let second = jobs.submit(backup_request("second")?).await?;
            let third = jobs.submit(backup_request("third")?).await?;
            let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
            let (stop, receiver) = watch::channel(false);
            let worker_jobs = jobs.clone();
            let worker = tokio::spawn(async move {
                worker_jobs.worker(receiver, move |request, directory| {
                    let (release, wait) = std::sync::mpsc::channel();
                    started.send((request, directory, release)).map_err(|error| JobError::internal(format!("cannot report operation start: {error}")))?;
                    wait.recv().map_err(|error| JobError::internal(format!("cannot wait for operation release: {error}")))?;
                    Ok(JobResult::Backup { backup_id: Uuid::new_v4().to_string() })
                }).await
            });
            let (request, working_directory, release_first) = starts.recv().await.ok_or("worker did not start")?;
            assert!(matches!(request, JobRequest::Backup { source, .. } if source == Path::new("first")));
            assert_eq!(working_directory, directory.path());
            assert!(starts.try_recv().is_err());
            assert!(matches!(jobs.get_job(&first.job_id).await?.ok_or("lost active job")?.state, JobState::Running));
            assert!(matches!(jobs.get_job(&second.job_id).await?.ok_or("lost queued job")?.state, JobState::Queued));
            // Querying and admitting work do not require releasing the operation.
            let fourth = jobs.submit(backup_request("fourth")?).await?;
            release_first.send(())?;
            let (request, _, release_second) = starts.recv().await.ok_or("worker did not start second job")?;
            assert!(matches!(request, JobRequest::Backup { source, .. } if source == Path::new("second")));
            stop.send(true)?;
            release_second.send(())?;
            worker.await??;
            assert!(starts.recv().await.is_none());
            for id in [&first.job_id, &second.job_id] {
                assert!(matches!(jobs.get_job(id).await?.ok_or("lost completed job")?.state, JobState::Succeeded(_)));
            }
            for id in [&third.job_id, &fourth.job_id] {
                assert!(matches!(jobs.get_job(id).await?.ok_or("lost queued job")?.state, JobState::Queued));
            }
            jobs.close().await;
            Ok::<_, Box<dyn Error>>(())
        }).await??;
        Ok(())
    }

    #[tokio::test]
    async fn idle_worker_wakes_for_new_jobs() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let jobs =
            JobManager::open(directory.path().to_owned(), crypto::Keyring::default()).await?;
        let (stop, receiver) = watch::channel(false);
        let worker_jobs = jobs.clone();
        let worker = tokio::spawn(async move { worker_jobs.run_worker(receiver).await });
        tokio::task::yield_now().await;
        let job = jobs.submit(backup_request("source")?).await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    jobs.get_job(&job.job_id)
                        .await?
                        .ok_or_else(|| JobError::internal("missing job"))?
                        .state,
                    JobState::Failed(_)
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            Ok::<_, JobError>(())
        })
        .await??;
        stop.send(true)?;
        worker.await??;
        jobs.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn admission_and_completion_failures_are_observable() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let jobs =
            JobManager::open(directory.path().to_owned(), crypto::Keyring::default()).await?;
        let job = jobs.submit(backup_request("source")?).await?;
        sqlx::raw_sql("CREATE TRIGGER block_completion BEFORE UPDATE ON jobs WHEN NEW.state IN ('failed', 'succeeded') BEGIN SELECT RAISE(FAIL, 'completion blocked'); END;").execute(&jobs.pool).await?;
        let (_stop, receiver) = watch::channel(false);
        let error = jobs
            .run_worker(receiver)
            .await
            .err()
            .ok_or("worker ignored completion failure")?;
        assert!(error.message.contains(&job.job_id));
        assert!(error.message.contains("completion blocked"));
        assert!(matches!(
            jobs.get_job(&job.job_id)
                .await?
                .ok_or("missing running job")?
                .state,
            JobState::Running
        ));
        sqlx::query("DROP TRIGGER block_completion")
            .execute(&jobs.pool)
            .await?;
        jobs.close().await;
        let error = jobs
            .submit(backup_request("not-admitted")?)
            .await
            .err()
            .ok_or("accepted job with unavailable database")?;
        assert!(error.message.contains("cannot queue job"));
        let jobs =
            JobManager::open(directory.path().to_owned(), crypto::Keyring::default()).await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM jobs")
                .fetch_one(&jobs.pool)
                .await?,
            1
        );
        assert!(matches!(
            jobs.get_job(&job.job_id)
                .await?
                .ok_or("lost interrupted job")?
                .state,
            JobState::Failed(_)
        ));
        jobs.close().await;
        Ok(())
    }
}

#[cfg(test)]
mod encryption_tests {
    use super::*;
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn old_job_schema_is_rejected_without_modification()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = crate::test_support::TestDirectory::new()?;
        let path = directory.path().join("jobs.sqlite3");
        let schema = storage::Schema {
            version: 1,
            sql: "CREATE TABLE sentinel (value TEXT); INSERT INTO sentinel VALUES ('preserve');",
        };
        let pool = storage::open(&path, schema, true).await?;
        pool.close().await;
        assert!(
            JobManager::open(directory.path().to_owned(), crypto::Keyring::default())
                .await
                .is_err()
        );
        let pool = storage::open(&path, schema, false).await?;
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT value FROM sentinel")
                .fetch_one(&pool)
                .await?,
            "preserve"
        );
        pool.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn queued_encryption_pins_key_across_restart() -> Result<(), Box<dyn std::error::Error>> {
        for scenario in ["same", "missing", "changed"] {
            let directory = crate::test_support::TestDirectory::new()?;
            let root = directory.path();
            std::fs::create_dir(root.join("source"))?;
            std::fs::create_dir(root.join("repository"))?;
            std::fs::write(root.join("source/file"), b"secret data")?;
            std::fs::write(root.join("key"), hex::encode([7; 32]))?;
            let files = BTreeMap::from([("main".to_string(), PathBuf::from("key"))]);
            let keys = crypto::Keyring::load(root, &files)?;
            let encryption = keys.select("main")?;
            let jobs = JobManager::open(root.to_owned(), keys).await?;
            let job = jobs
                .submit(JobRequest::Backup {
                    source: "source".into(),
                    repository: "repository".into(),
                    filter: Box::new(crate::test_support::unfiltered()?),
                    options: BackupOptions {
                        compression: true,
                        encryption: Some(encryption.clone()),
                    },
                })
                .await?;
            jobs.close().await;
            if scenario == "changed" {
                std::fs::write(root.join("key"), hex::encode([8; 32]))?;
            }
            let keys = if scenario == "missing" {
                crypto::Keyring::default()
            } else {
                crypto::Keyring::load(root, &files)?
            };
            let jobs = JobManager::open(root.to_owned(), keys).await?;
            let (stop, stopped) = watch::channel(false);
            let worker_jobs = jobs.clone();
            let worker = tokio::spawn(async move { worker_jobs.run_worker(stopped).await });
            let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let current = jobs
                        .get_job(&job.job_id)
                        .await?
                        .ok_or_else(|| JobError::internal("missing job"))?;
                    if !matches!(current.state, JobState::Queued | JobState::Running) {
                        return Ok::<_, JobError>(current);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await??;
            stop.send_replace(true);
            worker.await??;
            match outcome.state {
                JobState::Succeeded(_) => assert_eq!(scenario, "same"),
                JobState::Failed(error) => {
                    assert_ne!(scenario, "same", "{}", error.message);
                    assert_eq!(error.code, StatusCode::BAD_REQUEST);
                    assert!(!root.join("repository/backups").exists());
                }
                _ => return Err("job did not finish".into()),
            }
            let saved = serde_json::to_string(&outcome.request)?;
            assert!(saved.contains(&encryption.fingerprint));
            assert!(!saved.contains(&hex::encode([7; 32])));
            jobs.close().await;
        }
        Ok(())
    }
}
