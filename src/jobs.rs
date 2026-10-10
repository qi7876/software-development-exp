use crate::{
    backup::{self, BackupSummary},
    restore, storage,
};
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

const SCHEMA: &str = "
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
";

#[derive(Clone)]
pub(crate) struct JobManager {
    pool: SqlitePool,
    working_directory: PathBuf,
    wake: Arc<Notify>,
}

impl JobManager {
    pub(crate) async fn open(working_directory: PathBuf) -> Result<Self, JobError> {
        let pool = storage::open(&working_directory.join("jobs.sqlite3"), SCHEMA, true).await?;
        // A running operation may have performed external effects. Never replay it.
        sqlx::query("UPDATE jobs SET state = 'failed', error_code = 500, error_message = 'operation interrupted by service exit', finished_at = ? WHERE state = 'running'")
            .bind(timestamp()?).execute(&pool).await
            .map_err(|error| JobError::internal(format!("cannot recover interrupted jobs: {error}")))?;
        Ok(Self {
            pool,
            working_directory,
            wake: Arc::new(Notify::new()),
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
    ) -> Result<Vec<BackupSummary>, JobError> {
        let repository = self.working_directory.join(repository);
        tokio::task::spawn_blocking(move || backup::list(&repository))
            .await
            .map_err(|error| JobError::internal(format!("backup listing task failed: {error}")))?
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
        self.worker(stop, execute).await
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

fn execute(request: JobRequest, directory: PathBuf) -> Result<JobResult, JobError> {
    match request {
        JobRequest::Backup { source, repository } => {
            let backup_id = Uuid::new_v4().to_string();
            backup::run(
                &directory.join(source),
                &directory.join(repository),
                &backup_id,
            )?;
            Ok(JobResult::Backup { backup_id })
        }
        JobRequest::Restore {
            repository,
            backup_id,
            destination,
        } => {
            restore::run(
                &directory.join(repository),
                &backup_id,
                &directory.join(&destination),
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
    use crate::{api, test_support::TestDirectory};
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use std::{error::Error, time::Duration};
    use tower::ServiceExt;

    fn backup_request(source: &str) -> JobRequest {
        JobRequest::Backup {
            source: source.into(),
            repository: "repository".into(),
        }
    }

    #[tokio::test]
    async fn jobs_survive_restart_and_only_running_jobs_fail_recovery() -> Result<(), Box<dyn Error>>
    {
        let directory = TestDirectory::new()?;
        let jobs = JobManager::open(directory.path().to_owned()).await?;
        assert!(jobs.get_job("unknown").await?.is_none());
        let first = jobs.submit(backup_request("one")).await?;
        let second = jobs.submit(backup_request("two")).await?;
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
        let jobs = JobManager::open(directory.path().to_owned()).await?;
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
            execute(resumed.request, directory.path().to_owned()),
        )
        .await?;
        assert!(jobs.claim().await?.is_none());
        jobs.close().await;
        let jobs = JobManager::open(directory.path().to_owned()).await?;
        assert_eq!(
            jobs.get_job(&third.job_id)
                .await?
                .ok_or("lost failed result")?
                .to_json()?["error"]["code"],
            501
        );
        jobs.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn worker_serializes_operations_keeps_http_responsive_and_finishes_active_job_on_shutdown()
    -> Result<(), Box<dyn Error>> {
        tokio::time::timeout(Duration::from_secs(10), async {
            let directory = TestDirectory::new()?;
            let jobs = JobManager::open(directory.path().to_owned()).await?;
            let first = jobs.submit(backup_request("first")).await?;
            let second = jobs.submit(backup_request("second")).await?;
            let third = jobs.submit(backup_request("third")).await?;
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
            let app = api::router(jobs.clone(), "test-key".into());
            for path in ["/api/status".to_owned(), format!("/api/jobs/{}", first.job_id), format!("/api/jobs/{}", second.job_id)] {
                let request = Request::builder().uri(&path).header("Authorization", "Bearer test-key").body(Body::empty())?;
                let response = app.clone().oneshot(request).await?;
                assert_eq!(response.status(), StatusCode::OK);
                let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await?)?;
                if path.ends_with(&first.job_id) { assert_eq!(body["state"], "running"); }
                if path.ends_with(&second.job_id) { assert_eq!(body["state"], "queued"); }
            }
            // Querying and admitting work do not require releasing the operation.
            let fourth = jobs.submit(backup_request("fourth")).await?;
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
        let jobs = JobManager::open(directory.path().to_owned()).await?;
        let (stop, receiver) = watch::channel(false);
        let worker_jobs = jobs.clone();
        let worker = tokio::spawn(async move { worker_jobs.run_worker(receiver).await });
        tokio::task::yield_now().await;
        let job = jobs.submit(backup_request("source")).await?;
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
        let jobs = JobManager::open(directory.path().to_owned()).await?;
        let job = jobs.submit(backup_request("source")).await?;
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
            .submit(backup_request("not-admitted"))
            .await
            .err()
            .ok_or("accepted job with unavailable database")?;
        assert!(error.message.contains("cannot queue job"));
        let jobs = JobManager::open(directory.path().to_owned()).await?;
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
