use crate::{
    backup::{self, BackupSummary},
    restore,
};
use http::StatusCode;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub(crate) struct JobManager {
    job_infos: HashMap<String, JobInfo>,
    working_directory: PathBuf,
    log_file: Option<File>,
}

impl JobManager {
    pub(crate) fn new(working_directory: PathBuf, log_file: Option<File>) -> Self {
        Self {
            job_infos: HashMap::new(),
            working_directory,
            log_file,
        }
    }

    fn resolve_path(&self, path: &Path) -> PathBuf {
        self.working_directory.join(path)
    }

    pub(crate) fn start_backup(
        &mut self,
        source: PathBuf,
        repository: PathBuf,
    ) -> Result<JobInfo, JobError> {
        // TODO: validate paths before registering the job.
        let backup_id = Uuid::new_v4().to_string();
        let resolved_source = self.resolve_path(&source);
        let resolved_repository = self.resolve_path(&repository);
        let job_id = self.register_job(JobRequest::Backup { source, repository })?;
        let result = backup::run(&resolved_source, &resolved_repository, &backup_id)
            .map(|()| JobResult::Backup { backup_id });
        self.finish_job(&job_id, result)
    }

    pub(crate) fn start_restore(
        &mut self,
        repository: PathBuf,
        backup_id: String,
        destination: PathBuf,
    ) -> Result<JobInfo, JobError> {
        // TODO: validate paths before registering the job.
        let resolved_repository = self.resolve_path(&repository);
        let resolved_destination = self.resolve_path(&destination);
        let job_id = self.register_job(JobRequest::Restore {
            repository,
            backup_id: backup_id.clone(),
            destination: destination.clone(),
        })?;
        let result = restore::run(&resolved_repository, &backup_id, &resolved_destination)
            .map(|()| JobResult::Restore { destination });
        self.finish_job(&job_id, result)
    }

    pub(crate) fn list_backups(&self, repository: &Path) -> Result<Vec<BackupSummary>, JobError> {
        backup::list(&self.resolve_path(repository))
    }

    pub(crate) fn get_job(&self, job_id: &str) -> Option<&JobInfo> {
        self.job_infos.get(job_id)
    }

    fn register_job(&mut self, request: JobRequest) -> Result<String, JobError> {
        let job_id = Uuid::new_v4().to_string();
        let job = JobInfo::new(job_id.clone(), request);
        Self::record_job(self.log_file.as_mut(), &job)?;
        self.job_infos.insert(job_id.clone(), job);
        Ok(job_id)
    }

    fn finish_job(
        &mut self,
        job_id: &str,
        result: Result<JobResult, JobError>,
    ) -> Result<JobInfo, JobError> {
        let job = self.job_infos.get_mut(job_id).ok_or_else(|| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot update missing job {job_id}"),
            )
        })?;
        job.state = match result {
            Ok(result) => JobState::Succeeded(result),
            Err(error) => {
                eprintln!("{} job {job_id} failed: {}", job.kind_name(), error.message);
                JobState::Failed(error)
            }
        };
        // Keep the completed result available even if writing the log fails.
        Self::record_job(self.log_file.as_mut(), job)?;
        Ok(job.clone())
    }

    fn record_job(log_file: Option<&mut File>, job: &JobInfo) -> Result<(), JobError> {
        let Some(log_file) = log_file else {
            return Ok(());
        };
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| {
                JobError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("cannot timestamp task log: {error}"),
                )
            })?
            .as_millis();
        let request = serde_json::to_value(&job.request).map_err(|error| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot serialize task log request: {error}"),
            )
        })?;
        let record =
            json!({"timestamp_unix_ms": timestamp, "job": job.to_json()?, "request": request});
        let mut data = serde_json::to_vec(&record).map_err(|error| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot serialize task log: {error}"),
            )
        })?;
        data.push(b'\n');
        log_file.write_all(&data).map_err(|error| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot write task log for job {}: {error}", job.job_id),
            )
        })
    }
}

#[derive(Clone)]
pub(crate) struct JobInfo {
    pub(crate) job_id: String,
    pub(crate) request: JobRequest,
    pub(crate) state: JobState,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone)]
pub(crate) enum JobState {
    Running,
    Succeeded(JobResult),
    Failed(JobError),
}

#[derive(Clone)]
pub(crate) enum JobResult {
    Backup { backup_id: String },
    Restore { destination: std::path::PathBuf },
}

#[derive(Clone)]
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
}

impl JobInfo {
    pub(crate) fn kind_name(&self) -> &'static str {
        match self.request {
            JobRequest::Backup { .. } => "backup",
            JobRequest::Restore { .. } => "restore",
        }
    }

    pub(crate) fn state_name(&self) -> &'static str {
        match self.state {
            JobState::Running => "running",
            JobState::Succeeded(_) => "succeeded",
            JobState::Failed(_) => "failed",
        }
    }

    pub(crate) fn to_json(&self) -> Result<Value, JobError> {
        let kind = self.kind_name();
        let state = self.state_name();
        let (result, error) = match &self.state {
            JobState::Succeeded(JobResult::Backup { backup_id }) => {
                (json!({"backup_id": backup_id}), Value::Null)
            }
            JobState::Succeeded(JobResult::Restore { destination }) => {
                let destination = serde_json::to_value(destination).map_err(|error| {
                    JobError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("cannot serialize API response: {error}"),
                    )
                })?;
                (json!({"destination": destination}), Value::Null)
            }
            JobState::Failed(error) => (
                Value::Null,
                json!({"code": error.code.as_u16(), "message": error.message}),
            ),
            _ => (Value::Null, Value::Null),
        };
        Ok(json!({
            "job_id": self.job_id,
            "kind": kind,
            "state": state,
            "result": result,
            "error": error,
        }))
    }

    pub(crate) fn new(job_id: String, request: JobRequest) -> Self {
        JobInfo {
            job_id,
            request,
            state: JobState::Running,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn operations_finish_before_returning_and_remain_queryable() -> Result<(), Box<dyn Error>> {
        let mut jobs = JobManager::new(PathBuf::new(), None);
        assert!(jobs.get_job("unknown").is_none());
        let backup = jobs
            .start_backup("source".into(), "repository".into())
            .map_err(|error| error.message)?;
        let restore = jobs
            .start_restore("repository".into(), "backup-001".into(), "restored".into())
            .map_err(|error| error.message)?;
        assert_ne!(backup.job_id, restore.job_id);
        for job in [backup, restore] {
            assert!(
                matches!(job.state, JobState::Failed(ref error) if error.code == StatusCode::NOT_IMPLEMENTED)
            );
            let stored = jobs.get_job(&job.job_id).ok_or("missing completed job")?;
            assert_eq!(
                stored.to_json().map_err(|error| error.message)?,
                job.to_json().map_err(|error| error.message)?
            );
            assert!(
                JobManager::new(PathBuf::new(), None)
                    .get_job(&job.job_id)
                    .is_none()
            );
        }
        Ok(())
    }

    #[test]
    fn task_logs_append_started_and_completed_records() -> Result<(), Box<dyn Error>> {
        let directory = std::env::temp_dir().join(format!("bak-task-log-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        let path = directory.join("jobs.jsonl");
        let mut ids = Vec::new();
        for _ in 0..2 {
            let file = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&path)?;
            let mut jobs = JobManager::new(directory.clone(), Some(file));
            ids.push(
                jobs.start_backup("source".into(), "repository".into())
                    .map_err(|error| error.message)?
                    .job_id,
            );
        }
        let records: Vec<Value> = std::fs::read_to_string(&path)?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        assert_eq!(records.len(), 4);
        for (records, id) in records.as_chunks::<2>().0.iter().zip(ids) {
            assert_eq!(records[0]["job"]["state"], "running");
            assert_eq!(records[1]["job"]["state"], "failed");
            assert_eq!(records[1]["job"]["error"]["code"], 501);
            for record in records {
                assert_eq!(record["job"]["job_id"], id);
                assert_eq!(record["request"]["source"], "source");
                assert!(record["timestamp_unix_ms"].is_u64());
            }
        }
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn log_errors_prevent_registration_and_preserve_completed_results() -> Result<(), Box<dyn Error>>
    {
        let directory = std::env::temp_dir().join(format!("bak-task-log-error-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        let path = directory.join("jobs.jsonl");
        std::fs::write(&path, "")?;
        let mut jobs = JobManager::new(directory.clone(), Some(File::open(&path)?));
        let error = jobs
            .start_backup("source".into(), "repository".into())
            .err()
            .ok_or("accepted task despite unwritable task log")?;
        assert_eq!(error.code, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(error.message.contains("cannot write task log"));
        assert!(jobs.job_infos.is_empty());
        jobs.log_file = None;
        let id = jobs
            .register_job(JobRequest::Backup {
                source: "source".into(),
                repository: "repository".into(),
            })
            .map_err(|error| error.message)?;
        jobs.log_file = Some(File::open(&path)?);
        let error = jobs
            .finish_job(
                &id,
                Ok(JobResult::Backup {
                    backup_id: "backup-001".into(),
                }),
            )
            .err()
            .ok_or("ignored completion log error")?;
        assert!(error.message.contains(&id));
        assert!(matches!(
            jobs.get_job(&id).ok_or("missing completed job")?.state,
            JobState::Succeeded(_)
        ));
        assert_eq!(std::fs::read_to_string(&path)?, "");
        drop(jobs);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
