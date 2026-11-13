use crate::jobs::{
    BackupOptions, CompressionOptions, EncryptionRequest, Filter, JobError, JobManager, JobRequest,
};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use croner::{Cron, parser::CronParser};
use std::{collections::BTreeSet, path::PathBuf, time::Duration};
use tokio::sync::watch;

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScheduleConfig {
    timezone: String,
    #[serde(default)]
    plans: Vec<PlanConfig>,
}
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanConfig {
    id: String,
    #[serde(default = "enabled")]
    enabled: bool,
    cron: String,
    source: PathBuf,
    repository: PathBuf,
    #[serde(default)]
    filter: Filter,
    #[serde(default = "compression")]
    compression: Option<CompressionOptions>,
    encryption: Option<EncryptionRequest>,
}
fn enabled() -> bool {
    true
}
fn compression() -> Option<CompressionOptions> {
    BackupOptions::default().compression
}

pub(crate) struct Scheduler {
    timezone: Tz,
    plans: Vec<Plan>,
}
struct Plan {
    id: String,
    cron: Cron,
    request: JobRequest,
}

impl Scheduler {
    pub(crate) fn prepare(
        config: Option<ScheduleConfig>,
        keys: &crate::jobs::crypto::Keyring,
    ) -> Result<Self, JobError> {
        let Some(config) = config else {
            return Ok(Self {
                timezone: chrono_tz::UTC,
                plans: Vec::new(),
            });
        };
        let timezone = config
            .timezone
            .parse::<Tz>()
            .map_err(|e| JobError::internal(format!("scheduled_backups timezone: {e}")))?;
        let mut plans = Vec::new();
        let mut ids = BTreeSet::new();
        for plan in config.plans {
            let mut validate = || -> Result<Cron, JobError> {
                if plan.id.trim().is_empty() || !ids.insert(plan.id.clone()) {
                    return Err(JobError::internal("plan id must be nonempty and unique"));
                }
                if plan.source.as_os_str().is_empty() || plan.repository.as_os_str().is_empty() {
                    return Err(JobError::internal(
                        "source and repository must not be empty",
                    ));
                }
                let fields: Vec<_> = plan.cron.split_whitespace().collect();
                if fields.len() != 5
                    || !plan.cron.chars().all(|c| {
                        c.is_ascii_digit() || c.is_ascii_whitespace() || "*,-/".contains(c)
                    })
                {
                    return Err(JobError::internal(
                        "cron requires five numeric fields (minute hour day month weekday)",
                    ));
                }
                let cron = CronParser::new()
                    .parse(&plan.cron)
                    .map_err(|e| JobError::internal(format!("invalid cron: {e}")))?;
                plan.filter.compile()?;
                if let Some(options) = &plan.compression {
                    options.validate()?;
                }
                Ok(cron)
            };
            let cron =
                validate().map_err(|e| JobError::internal(format!("schedule {}: {e}", plan.id)))?;
            if !plan.enabled {
                continue;
            }
            let encryption = plan
                .encryption
                .as_ref()
                .map(|e| e.resolve(keys))
                .transpose()
                .map_err(|e| JobError::internal(format!("schedule {}: {e}", plan.id)))?;
            plans.push(Plan {
                id: plan.id,
                cron,
                request: JobRequest::Backup {
                    source: plan.source,
                    repository: plan.repository,
                    filter: Box::new(plan.filter),
                    options: BackupOptions {
                        compression: plan.compression,
                        encryption,
                    },
                },
            });
        }
        Ok(Self { timezone, plans })
    }
    pub(crate) async fn initialize(
        &self,
        jobs: &JobManager,
        now: DateTime<Utc>,
    ) -> Result<(), JobError> {
        for plan in &self.plans {
            jobs.initialize_schedule(&plan.id, now.timestamp().div_euclid(60))
                .await?;
        }
        Ok(())
    }
    async fn tick(&self, jobs: &JobManager, now: DateTime<Utc>) -> Result<(), JobError> {
        let minute = now.timestamp().div_euclid(60);
        let time = DateTime::from_timestamp(minute * 60, 0)
            .ok_or_else(|| JobError::internal("schedule time out of range"))?
            .with_timezone(&self.timezone);
        for plan in &self.plans {
            let matches = plan
                .cron
                .is_time_matching(&time)
                .map_err(|e| JobError::internal(format!("schedule {}: {e}", plan.id)))?;
            jobs.schedule_tick(&plan.id, minute, matches.then_some(&plan.request))
                .await?;
        }
        Ok(())
    }
    pub(crate) async fn run(
        self,
        jobs: JobManager,
        mut stop: watch::Receiver<bool>,
    ) -> Result<(), JobError> {
        let mut last_minute = Utc::now().timestamp().div_euclid(60);
        loop {
            if *stop.borrow() {
                return Ok(());
            }
            tokio::select! {
                biased;
                changed = stop.changed() => { if changed.is_err() || *stop.borrow() { return Ok(()); } }
                () = tokio::time::sleep(Duration::from_secs(1)) => {
                    let now = Utc::now();
                    let minute = now.timestamp().div_euclid(60);
                    if minute > last_minute { self.tick(&jobs, now).await?; last_minute = minute; }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
    fn config(cron: &str, timezone: &str) -> Value {
        json!({"timezone":timezone,"plans":[{"id":"daily","cron":cron,"source":"source","repository":"repo"}]})
    }
    fn scheduler(value: Value) -> Result<Scheduler> {
        Ok(Scheduler::prepare(
            Some(serde_json::from_value(value)?),
            &crate::jobs::crypto::Keyring::default(),
        )?)
    }
    fn time(value: &str) -> Result<DateTime<Utc>> {
        Ok(value.parse()?)
    }

    #[test]
    fn configuration_and_calendar_matching() -> Result {
        assert!(
            Scheduler::prepare(None, &crate::jobs::crypto::Keyring::default())?
                .plans
                .is_empty()
        );
        assert!(
            scheduler(json!({"timezone":"UTC","plans":[]}))?
                .plans
                .is_empty()
        );
        for cron in [
            "* * * *",
            "0 * * * * *",
            "@daily",
            "0 2 * * MON",
            "60 2 * * *",
            "0 25 * * *",
            "* * * * ?",
        ] {
            assert!(scheduler(config(cron, "UTC")).is_err(), "{cron}");
        }
        for (field, value) in [
            ("id", json!("")),
            ("source", json!("")),
            ("repository", json!("")),
            ("compression", json!({"algorithm":"zstd","level":0})),
            (
                "encryption",
                json!({"algorithm":"aes-256-gcm","key_id":"missing"}),
            ),
            ("extra", json!(true)),
            ("filter", json!({"bad":true})),
        ] {
            let mut value_config = config("0 2 * * *", "UTC");
            value_config["plans"][0][field] = value;
            assert!(scheduler(value_config).is_err(), "{field}");
        }
        let mut duplicate = config("0 2 * * *", "UTC");
        let plan = duplicate["plans"][0].clone();
        duplicate["plans"].as_array_mut().ok_or("plans")?.push(plan);
        assert!(scheduler(duplicate).is_err());
        assert!(scheduler(config("0 2 * * *", "Invalid/Zone")).is_err());
        let mut disabled = config("0 2 * * *", "UTC");
        disabled["plans"][0]["enabled"] = json!(false);
        assert!(scheduler(disabled)?.plans.is_empty());
        for (cron, zone, yes, no) in [
            (
                "0 2 * * *",
                "Asia/Shanghai",
                "2026-11-12T18:00:00Z",
                "2026-11-12T02:00:00Z",
            ),
            (
                "0 2 * * 1",
                "UTC",
                "2026-11-16T02:00:00Z",
                "2026-11-17T02:00:00Z",
            ),
            (
                "0 2 1 * *",
                "UTC",
                "2026-12-01T02:00:00Z",
                "2026-12-02T02:00:00Z",
            ),
            (
                "0 2 1 * 1",
                "UTC",
                "2026-11-16T02:00:00Z",
                "2026-11-17T02:00:00Z",
            ),
        ] {
            let scheduler = scheduler(config(cron, zone))?;
            assert!(
                scheduler.plans[0]
                    .cron
                    .is_time_matching(&time(yes)?.with_timezone(&scheduler.timezone))?
            );
            assert!(
                !scheduler.plans[0]
                    .cron
                    .is_time_matching(&time(no)?.with_timezone(&scheduler.timezone))?
            );
        }
        // Spring's missing local 02:30 has no matching UTC minute.
        let spring = scheduler(config("30 2 * * *", "America/New_York"))?;
        for minute in 0..180 {
            let utc = time("2026-03-08T06:00:00Z")? + chrono::Duration::minutes(minute);
            assert!(
                !spring.plans[0]
                    .cron
                    .is_time_matching(&utc.with_timezone(&spring.timezone))?
            );
        }
        let scheduler = scheduler(config("30 1 * * *", "America/New_York"))?;
        for utc in ["2026-11-01T05:30:00Z", "2026-11-01T06:30:00Z"] {
            assert!(
                scheduler.plans[0]
                    .cron
                    .is_time_matching(&time(utc)?.with_timezone(&scheduler.timezone))?
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn durable_ticks_skip_busy_work_and_restart_without_catchup() -> Result {
        let directory = crate::test_support::TestDirectory::new()?;
        let jobs = JobManager::open(
            directory.path().to_owned(),
            crate::jobs::crypto::Keyring::default(),
        )
        .await?;
        let scheduler = scheduler(config("* * * * *", "UTC"))?;
        let now = time("2026-11-13T00:00:00Z")?;
        scheduler.initialize(&jobs, now).await?;
        scheduler.tick(&jobs, now).await?;
        assert_eq!(
            jobs.list_jobs(100, None).await?["jobs"]
                .as_array()
                .ok_or("jobs")?
                .len(),
            0
        );
        scheduler
            .tick(&jobs, now + chrono::Duration::minutes(1))
            .await?;
        scheduler
            .tick(&jobs, now + chrono::Duration::minutes(1))
            .await?;
        scheduler
            .tick(&jobs, now + chrono::Duration::minutes(2))
            .await?;
        let history = jobs.list_jobs(100, None).await?;
        assert_eq!(history["jobs"].as_array().ok_or("jobs")?.len(), 1);
        assert_eq!(history["jobs"][0]["schedule_id"], "daily");
        assert_eq!(
            history["jobs"][0]["scheduled_for"],
            (now + chrono::Duration::minutes(1)).timestamp_millis()
        );
        assert_eq!(
            history["jobs"][0]["request"]["compression"],
            json!({"algorithm":"zstd","level":3})
        );
        let id = history["jobs"][0]["job_id"].as_str().ok_or("job_id")?;
        assert_eq!(
            jobs.get_job(id).await?.ok_or("job")?.to_json()?["schedule_id"],
            "daily"
        );
        jobs.close().await;
        let jobs = JobManager::open(
            directory.path().to_owned(),
            crate::jobs::crypto::Keyring::default(),
        )
        .await?;
        scheduler
            .initialize(&jobs, now + chrono::Duration::minutes(10))
            .await?;
        scheduler
            .tick(&jobs, now + chrono::Duration::minutes(11))
            .await?;
        assert_eq!(
            jobs.list_jobs(100, None).await?["jobs"]
                .as_array()
                .ok_or("jobs")?
                .len(),
            1
        );
        // Missing source fails normally; failure releases the plan for its next occurrence.
        let (stop, rx) = watch::channel(false);
        let worker_jobs = jobs.clone();
        let worker = tokio::spawn(async move { worker_jobs.run_worker(rx).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if jobs
                    .get_job(id)
                    .await?
                    .ok_or_else(|| JobError::internal("missing job"))?
                    .state_name()
                    == "failed"
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
            Ok::<_, JobError>(())
        })
        .await??;
        stop.send_replace(true);
        worker.await??;
        scheduler
            .tick(&jobs, now + chrono::Duration::minutes(11))
            .await?;
        scheduler
            .tick(&jobs, now + chrono::Duration::minutes(3))
            .await?;
        assert_eq!(
            jobs.list_jobs(100, None).await?["jobs"]
                .as_array()
                .ok_or("jobs")?
                .len(),
            1
        );
        scheduler
            .tick(&jobs, now + chrono::Duration::minutes(20))
            .await?;
        assert_eq!(
            jobs.list_jobs(100, None).await?["jobs"]
                .as_array()
                .ok_or("jobs")?
                .len(),
            2
        );
        // A closed database must surface an error instead of silently losing triggers.
        jobs.close().await;
        assert!(
            scheduler
                .tick(&jobs, now + chrono::Duration::minutes(21))
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn scheduled_backup_restores_and_scheduler_stops() -> Result {
        let directory = crate::test_support::TestDirectory::new()?;
        std::fs::create_dir(directory.path().join("source"))?;
        std::fs::create_dir(directory.path().join("repo"))?;
        std::fs::create_dir(directory.path().join("restored"))?;
        std::fs::write(directory.path().join("source/file"), b"scheduled contents")?;
        let jobs = JobManager::open(
            directory.path().to_owned(),
            crate::jobs::crypto::Keyring::default(),
        )
        .await?;
        let mut value = config("* * * * *", "UTC");
        let mut second = value["plans"][0].clone();
        second["id"] = json!("second");
        value["plans"].as_array_mut().ok_or("plans")?.push(second);
        let scheduler = scheduler(value)?;
        let now = time("2026-11-13T00:00:00Z")?;
        scheduler.initialize(&jobs, now).await?;
        scheduler
            .tick(&jobs, now + chrono::Duration::minutes(1))
            .await?;
        let (stop, rx) = watch::channel(false);
        let worker_jobs = jobs.clone();
        let worker = tokio::spawn(async move { worker_jobs.run_worker(rx).await });
        let backup_id = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let list = jobs.list_jobs(100, None).await?;
                if list["jobs"].as_array().is_some_and(|items| {
                    items.len() == 2 && items.iter().all(|j| j["state"] == "succeeded")
                }) {
                    return Ok::<_, JobError>(
                        list["jobs"][0]["result"]["backup_id"]
                            .as_str()
                            .ok_or_else(|| JobError::internal("backup_id"))?
                            .to_string(),
                    );
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        let restore = jobs
            .submit(JobRequest::Restore {
                repository: "repo".into(),
                backup_id,
                destination: "restored".into(),
            })
            .await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if jobs
                    .get_job(&restore.job_id)
                    .await?
                    .ok_or_else(|| JobError::internal("job"))?
                    .state_name()
                    == "succeeded"
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
            Ok::<_, JobError>(())
        })
        .await??;
        assert_eq!(
            std::fs::read(directory.path().join("restored/file"))?,
            b"scheduled contents"
        );
        stop.send_replace(true);
        worker.await??;
        scheduler.run(jobs.clone(), stop.subscribe()).await?;
        jobs.close().await;
        Ok(())
    }
}
