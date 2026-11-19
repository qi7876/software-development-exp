use super::{
    JobError, JobInfo, JobManager, JobRequest, JobResult, archive,
    content::{BackupOptions, Keyring},
    filesystem::sync_directory,
    repository::ArchiveFile,
};
use std::{
    fs::File,
    path::{Path, PathBuf},
};

impl JobManager {
    pub(super) async fn recover_interrupted(&self) -> Result<(), JobError> {
        let rows = sqlx::query("SELECT * FROM jobs WHERE state = 'running' ORDER BY sequence")
            .fetch_all(&self.pool)
            .await?;
        for row in rows {
            let job = JobInfo::from_row(row)?;
            let id = job.job_id.clone();
            println!("Reconciling interrupted {} job {id}", job.kind_name());
            let result = match job.request {
                JobRequest::Backup {
                    repository,
                    options,
                    ..
                } => {
                    let directory = self.working_directory.join(repository);
                    let keys = self.keys.clone();
                    let backup_id = id.clone();
                    tokio::task::spawn_blocking(move || reconcile_backup(&directory, &backup_id, &options, &keys))
                        .await.map_err(|error| JobError::internal(format!("backup recovery task failed: {error}")))?
                        .map_err(|error| JobError::internal(format!("operation interrupted by service exit; backup reconciliation failed: {}", error.message)))
                }
                JobRequest::Restore { .. } => Err(JobError::internal(
                    "operation interrupted by service exit; restore output was preserved and was not replayed",
                )),
            };
            let recovered = result.is_ok();
            self.finish(&id, result).await?;
            if recovered {
                println!("Recovered backup job {id}: verified and synchronized published archive");
            }
        }
        Ok(())
    }
}

fn pending_paths(directory: &Path, id: &str) -> Result<Vec<PathBuf>, JobError> {
    let prefix = format!(".{id}.");
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(attempt) = name
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
            .and_then(|name| name.strip_suffix(".partial"))
        else {
            continue;
        };
        if uuid::Uuid::parse_str(attempt).is_ok() && entry.file_type()?.is_file() {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn verify(
    path: &Path,
    id: &str,
    options: &BackupOptions,
    keys: &Keyring,
) -> Result<ArchiveFile, JobError> {
    let backup = ArchiveFile::open_expected(path, id)?;
    if backup.manifest.compression != options.compression
        || backup.manifest.encryption != options.encryption
    {
        return Err(JobError::internal(
            "archive settings do not match the persisted job request",
        ));
    }
    backup.verify(keys)?;
    backup.file.sync_all()?;
    Ok(backup)
}

#[cfg(unix)]
fn same_inode(file: &File, path: &Path) -> Result<bool, JobError> {
    use std::os::unix::fs::MetadataExt;
    let expected = file.metadata()?;
    let actual = std::fs::symlink_metadata(path)?;
    Ok(actual.is_file() && expected.dev() == actual.dev() && expected.ino() == actual.ino())
}

#[cfg(not(unix))]
fn same_inode(_: &File, _: &Path) -> Result<bool, JobError> {
    Ok(false)
}

fn reconcile_backup(
    directory: &Path,
    id: &str,
    options: &BackupOptions,
    keys: &Keyring,
) -> Result<JobResult, JobError> {
    let destination = archive::path(directory, id);
    let pending = pending_paths(directory, id)?;
    let published = match std::fs::symlink_metadata(&destination) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let backup = if published {
        verify(&destination, id, options, keys)?
    } else {
        // Never guess which attempt to publish when more than one candidate exists.
        let [candidate] = pending.as_slice() else {
            return Err(JobError::internal(if pending.is_empty() {
                "no published archive or owned partial was found; no backup was replayed"
            } else {
                "multiple partial archives found; files were retained without choosing an attempt"
            }));
        };
        let backup = verify(candidate, id, options, keys)?;
        std::fs::hard_link(candidate, &destination).map_err(|error| {
            JobError::internal(format!(
                "cannot publish recovered backup without overwriting {}: {error}",
                destination.display()
            ))
        })?;
        backup
    };
    sync_directory(directory)?;
    for path in pending {
        match same_inode(&backup.file, &path) {
            Ok(true) => {
                if let Err(error) = std::fs::remove_file(&path) {
                    eprintln!(
                        "Cannot remove recovered temporary link {}: {error}",
                        path.display()
                    );
                }
            }
            Ok(false) => eprintln!(
                "Retained unconfirmed partial backup for job {id}: {}",
                path.display()
            ),
            Err(error) => eprintln!(
                "Cannot inspect partial backup {}; retained: {error}",
                path.display()
            ),
        }
    }
    // The final name is already durable. Cleanup failure must not invalidate it.
    if let Err(error) = sync_directory(directory) {
        eprintln!("Cannot synchronize temporary-link cleanup for job {id}: {error}");
    }
    Ok(JobResult::Backup {
        backup_id: id.to_owned(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        jobs::{self, CompressionOptions, EncryptionAlgorithm, JobState},
        test_support::{TestDirectory, unfiltered},
    };
    use serde_json::json;
    use std::collections::BTreeMap;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn request(options: BackupOptions) -> Result<JobRequest, serde_json::Error> {
        Ok(JobRequest::Backup {
            source: "source".into(),
            repository: "repo".into(),
            filter: Box::new(unfiltered()?),
            options,
        })
    }

    fn fixture() -> Result<TestDirectory, Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        for name in ["source", "repo", "destination"] {
            std::fs::create_dir(directory.path().join(name))?;
        }
        std::fs::write(
            directory.path().join("source/file"),
            b"recovered backup contents",
        )?;
        Ok(directory)
    }

    async fn produce(
        jobs: &JobManager,
        options: BackupOptions,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let admitted = jobs.submit(request(options)?).await?;
        let active = jobs.claim().await?.ok_or("missing claimed job")?;
        assert_eq!(active.job_id, admitted.job_id);
        let result = jobs::execute(
            active.request,
            jobs.working_directory.clone(),
            &jobs.keys,
            &active.job_id,
        )?;
        assert!(
            matches!(result, JobResult::Backup { ref backup_id } if backup_id == &active.job_id)
        );
        Ok(active.job_id)
    }

    fn partial(directory: &Path, id: &str) -> PathBuf {
        directory
            .join("repo")
            .join(format!(".{id}.{}.partial", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn published_output_is_recovered_without_source_and_second_restart_is_idempotent()
    -> TestResult {
        let directory = fixture()?;
        let jobs = JobManager::open(directory.path().to_owned(), Keyring::default()).await?;
        let id = produce(&jobs, BackupOptions::default()).await?;
        let queued = jobs.submit(request(BackupOptions::default())?).await?;
        let path = archive::path(&directory.path().join("repo"), &id);
        let original = std::fs::read(&path)?;
        let original_request = jobs.list_jobs(20, None).await?["jobs"][1]["request"].clone();
        let same = partial(directory.path(), &id);
        std::fs::hard_link(&path, &same)?;
        let independent = partial(directory.path(), &id);
        std::fs::copy(&path, &independent)?;
        let unknown = partial(directory.path(), "unrelated");
        std::fs::write(&unknown, b"do not remove")?;
        std::fs::remove_dir_all(directory.path().join("source"))?;
        jobs.close().await;

        let jobs = JobManager::open(directory.path().to_owned(), Keyring::default()).await?;
        let recovered = jobs
            .get_job(&id)
            .await?
            .ok_or("missing recovered job")?
            .to_json()?;
        assert_eq!(recovered["state"], "succeeded");
        assert_eq!(recovered["result"]["backup_id"], id);
        assert!(matches!(
            jobs.get_job(&queued.job_id)
                .await?
                .ok_or("missing queued job")?
                .state,
            JobState::Queued
        ));
        assert!(!same.exists());
        assert_eq!(std::fs::read(&independent)?, original);
        assert_eq!(std::fs::read(&unknown)?, b"do not remove");
        assert_eq!(std::fs::read(&path)?, original);
        let history = jobs.list_jobs(20, None).await?;
        assert_eq!(history["jobs"][1]["request"], original_request);
        jobs.close().await;
        let jobs = JobManager::open(directory.path().to_owned(), Keyring::default()).await?;
        assert_eq!(jobs.list_jobs(20, None).await?, history);
        jobs.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn complete_partials_are_promoted_for_all_storage_modes() -> TestResult {
        for compression in [
            None,
            Some(CompressionOptions::Zstd(Default::default())),
            Some(CompressionOptions::Lz77(Default::default())),
        ] {
            for algorithm in [
                None,
                Some(EncryptionAlgorithm::XChaCha20Poly1305),
                Some(EncryptionAlgorithm::Aes256Gcm),
            ] {
                let directory = fixture()?;
                let source = directory.path().join("source");
                std::fs::hard_link(source.join("file"), source.join("alias"))?;
                nix::unistd::mkfifo(&source.join("pipe"), nix::sys::stat::Mode::S_IRUSR)?;
                std::fs::write(directory.path().join("key"), hex::encode([7; 32]))?;
                let files = BTreeMap::from([("current".to_owned(), PathBuf::from("key"))]);
                let keys = Keyring::load(directory.path(), &files)?;
                let encryption = algorithm
                    .map(|algorithm| keys.select("current", algorithm))
                    .transpose()?;
                let jobs = JobManager::open(directory.path().to_owned(), keys).await?;
                let id = produce(
                    &jobs,
                    BackupOptions {
                        compression: compression.clone(),
                        encryption,
                    },
                )
                .await?;
                let path = archive::path(&directory.path().join("repo"), &id);
                let original = std::fs::read(&path)?;
                let temporary = partial(directory.path(), &id);
                std::fs::rename(&path, &temporary)?;
                jobs.close().await;
                let jobs = JobManager::open(
                    directory.path().to_owned(),
                    Keyring::load(directory.path(), &files)?,
                )
                .await?;
                assert_eq!(
                    jobs.get_job(&id)
                        .await?
                        .ok_or("missing recovered job")?
                        .state_name(),
                    "succeeded"
                );
                assert_eq!(std::fs::read(&path)?, original);
                assert!(!temporary.exists());
                jobs::restore::run_with_keys(
                    &directory.path().join("repo"),
                    &id,
                    &directory.path().join("destination"),
                    &jobs.keys,
                )?;
                assert_eq!(
                    std::fs::read(directory.path().join("destination/file"))?,
                    b"recovered backup contents"
                );
                jobs.close().await;
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn incomplete_ambiguous_mismatched_and_foreign_output_is_preserved_without_success()
    -> TestResult {
        for scenario in [
            "incomplete",
            "ambiguous",
            "corrupt",
            "unsupported version",
            "settings",
            "foreign",
            "missing",
        ] {
            let directory = fixture()?;
            let jobs = JobManager::open(directory.path().to_owned(), Keyring::default()).await?;
            let id = produce(&jobs, BackupOptions::default()).await?;
            let path = archive::path(&directory.path().join("repo"), &id);
            let candidate = partial(directory.path(), &id);
            match scenario {
                "incomplete" => {
                    std::fs::remove_file(&path)?;
                    std::fs::write(&candidate, b"BAKFILE\x01unfinished")?;
                }
                "ambiguous" => {
                    std::fs::copy(&path, partial(directory.path(), &id))?;
                    std::fs::rename(&path, &candidate)?;
                }
                "corrupt" => {
                    let mut bytes = std::fs::read(&path)?;
                    bytes[8] ^= 1;
                    std::fs::write(&path, bytes)?;
                }
                "unsupported version" => {
                    crate::test_support::rewrite_backup_metadata(&path, |manifest| {
                        manifest["format_version"] = json!(9);
                    })?;
                }
                "settings" => {
                    sqlx::query("UPDATE jobs SET request = json_set(request, '$.compression', null) WHERE job_id = ?").bind(&id).execute(&jobs.pool).await?;
                }
                "foreign" => {
                    let other = "unrelated-backup";
                    jobs::backup::run(
                        &directory.path().join("source"),
                        &directory.path().join("repo"),
                        &unfiltered()?,
                        other,
                    )?;
                    std::fs::remove_file(&path)?;
                }
                _ => {
                    std::fs::remove_file(&path)?;
                }
            }
            let before: BTreeMap<_, _> = std::fs::read_dir(directory.path().join("repo"))?
                .map(|entry| {
                    let entry = entry?;
                    Ok((entry.path(), std::fs::read(entry.path())?))
                })
                .collect::<Result<_, std::io::Error>>()?;
            let queued = jobs.submit(request(BackupOptions::default())?).await?;
            jobs.close().await;
            let jobs = JobManager::open(directory.path().to_owned(), Keyring::default()).await?;
            let job = jobs
                .get_job(&id)
                .await?
                .ok_or("lost interrupted job")?
                .to_json()?;
            assert_eq!(job["state"], "failed", "{scenario}: {job}");
            let message = job["error"]["message"].as_str().ok_or("missing error")?;
            assert!(message.contains("interrupted"));
            if scenario == "unsupported version" {
                assert!(message.contains("unsupported backup format version 9"));
            }
            assert!(matches!(
                jobs.get_job(&queued.job_id)
                    .await?
                    .ok_or("lost queued job")?
                    .state,
                JobState::Queued
            ));
            for (path, bytes) in before {
                assert_eq!(std::fs::read(path)?, bytes);
            }
            jobs.close().await;
        }
        Ok(())
    }

    #[tokio::test]
    async fn missing_key_fails_closed_and_keeps_the_encrypted_archive() -> TestResult {
        let directory = fixture()?;
        std::fs::write(directory.path().join("key"), hex::encode([7; 32]))?;
        let keys = Keyring::load(
            directory.path(),
            &BTreeMap::from([("current".into(), "key".into())]),
        )?;
        let encryption = keys.select("current", EncryptionAlgorithm::Aes256Gcm)?;
        let jobs = JobManager::open(directory.path().to_owned(), keys).await?;
        let id = produce(
            &jobs,
            BackupOptions {
                compression: None,
                encryption: Some(encryption),
            },
        )
        .await?;
        let path = archive::path(&directory.path().join("repo"), &id);
        let original = std::fs::read(&path)?;
        jobs.close().await;
        let jobs = JobManager::open(directory.path().to_owned(), Keyring::default()).await?;
        let job = jobs.get_job(&id).await?.ok_or("missing job")?.to_json()?;
        assert_eq!(job["state"], "failed");
        assert!(
            job["error"]["message"]
                .as_str()
                .ok_or("missing error")?
                .contains("not configured")
        );
        assert_eq!(std::fs::read(path)?, original);
        jobs.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn recovery_commit_failure_leaves_running_for_a_later_retry() -> TestResult {
        let directory = fixture()?;
        let jobs = JobManager::open(directory.path().to_owned(), Keyring::default()).await?;
        let id = produce(&jobs, BackupOptions::default()).await?;
        let path = archive::path(&directory.path().join("repo"), &id);
        let temporary = partial(directory.path(), &id);
        std::fs::rename(&path, &temporary)?;
        sqlx::raw_sql("CREATE TRIGGER reject_recovery BEFORE UPDATE ON jobs BEGIN SELECT RAISE(ABORT, 'injected recovery commit failure'); END;").execute(&jobs.pool).await?;
        jobs.close().await;
        let error = JobManager::open(directory.path().to_owned(), Keyring::default())
            .await
            .err()
            .ok_or("accepted failed recovery commit")?;
        assert!(error.message.contains("injected recovery commit failure"));
        assert!(path.exists());
        assert!(!temporary.exists());
        let pool = sqlx::SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(directory.path().join("jobs.sqlite3")),
        )
        .await?;
        let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE job_id = ?")
            .bind(&id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(state, "running");
        sqlx::query("DROP TRIGGER reject_recovery")
            .execute(&pool)
            .await?;
        pool.close().await;
        let jobs = JobManager::open(directory.path().to_owned(), Keyring::default()).await?;
        assert_eq!(
            jobs.get_job(&id)
                .await?
                .ok_or("lost recovered job")?
                .state_name(),
            "succeeded"
        );
        jobs.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn interrupted_restore_never_replays_or_removes_existing_output() -> TestResult {
        let directory = fixture()?;
        let jobs = JobManager::open(directory.path().to_owned(), Keyring::default()).await?;
        let job = jobs
            .submit(JobRequest::Restore {
                repository: "repo".into(),
                backup_id: "backup".into(),
                destination: "destination".into(),
            })
            .await?;
        jobs.claim().await?.ok_or("missing restore")?;
        std::fs::write(directory.path().join("destination/partial"), b"preserve")?;
        jobs.close().await;
        let jobs = JobManager::open(directory.path().to_owned(), Keyring::default()).await?;
        let info = jobs
            .get_job(&job.job_id)
            .await?
            .ok_or("lost restore")?
            .to_json()?;
        assert_eq!(info["state"], "failed");
        assert_eq!(
            std::fs::read(directory.path().join("destination/partial"))?,
            b"preserve"
        );
        assert_eq!(info["result"], json!(null));
        jobs.close().await;
        Ok(())
    }
}
