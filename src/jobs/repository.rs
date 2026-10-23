use super::{JobError, filesystem::get_absolute_directory_path, storage};
use http::StatusCode;
use sqlx::Row;
use std::path::{Path, PathBuf};

#[derive(serde::Serialize)]
pub struct RepoBackupInfo {
    pub backup_id: String,
    pub created_at: String,
}

pub const REPO_SCHEMA: storage::Schema = storage::Schema {
    version: 2,
    sql: "
CREATE TABLE backups (
    backup_id TEXT PRIMARY KEY NOT NULL,
    format_version INTEGER NOT NULL CHECK (format_version = 2),
    created_at TEXT NOT NULL,
    source TEXT NOT NULL,
    compression TEXT NOT NULL,
    checksum_algorithm TEXT NOT NULL,
    root_uid INTEGER NOT NULL CHECK (root_uid BETWEEN 0 AND 4294967295),
    root_gid INTEGER NOT NULL CHECK (root_gid BETWEEN 0 AND 4294967295),
    root_mode INTEGER NOT NULL CHECK (root_mode BETWEEN 0 AND 4095),
    root_atime_seconds INTEGER NOT NULL,
    root_atime_nanoseconds INTEGER NOT NULL CHECK (root_atime_nanoseconds BETWEEN 0 AND 999999999),
    root_mtime_seconds INTEGER NOT NULL,
    root_mtime_nanoseconds INTEGER NOT NULL CHECK (root_mtime_nanoseconds BETWEEN 0 AND 999999999),
    entries_created INTEGER NOT NULL CHECK (entries_created >= 0),
    skipped_unsupported INTEGER NOT NULL CHECK (skipped_unsupported >= 0),
    skipped_filtered INTEGER NOT NULL CHECK (skipped_filtered >= 0)
);
CREATE TABLE entries (
    backup_id TEXT NOT NULL REFERENCES backups(backup_id),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    type TEXT NOT NULL CHECK (type IN ('file', 'directory', 'symlink')),
    path TEXT NOT NULL,
    uid INTEGER NOT NULL CHECK (uid BETWEEN 0 AND 4294967295),
    gid INTEGER NOT NULL CHECK (gid BETWEEN 0 AND 4294967295),
    mode INTEGER NOT NULL CHECK (mode BETWEEN 0 AND 4095),
    atime_seconds INTEGER NOT NULL,
    atime_nanoseconds INTEGER NOT NULL CHECK (atime_nanoseconds BETWEEN 0 AND 999999999),
    mtime_seconds INTEGER NOT NULL,
    mtime_nanoseconds INTEGER NOT NULL CHECK (mtime_nanoseconds BETWEEN 0 AND 999999999),
    original_size INTEGER,
    checksum TEXT,
    link_target TEXT,
    PRIMARY KEY (backup_id, ordinal),
    UNIQUE (backup_id, path),
    CHECK (
        (type = 'file' AND original_size IS NOT NULL AND original_size >= 0 AND checksum IS NOT NULL AND link_target IS NULL) OR
        (type = 'directory' AND original_size IS NULL AND checksum IS NULL AND link_target IS NULL) OR
        (type = 'symlink' AND original_size IS NULL AND checksum IS NULL AND link_target IS NOT NULL)
    )
);
",
};

// Repository metadata is committed once and is immutable thereafter.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub backup_id: String,
    pub created_at: String,
    pub source: PathBuf,
    pub compression_algorithm: String,
    pub checksum_algorithm: String,
    pub entries: Vec<ManifestEntry>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum ManifestEntry {
    File {
        path: PathBuf,
        original_size: u64,
        checksum: String,
    },
    Directory {
        path: PathBuf,
    },
}

impl Manifest {
    pub async fn read(repository: &Path, backup_id: &str) -> Result<Self, JobError> {
        let path = repository.join("repository.sqlite3");
        let pool = storage::open(&path, REPO_SCHEMA, false).await?;
        let result = async {
            let mut transaction = pool.begin().await?;
            let row = sqlx::query("SELECT * FROM backups WHERE backup_id = ?")
                .bind(backup_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| {
                    JobError::new(
                        StatusCode::NOT_FOUND,
                        format!("backup {backup_id} not found"),
                    )
                })?;
            let mut manifest = Self {
                format_version: u32::try_from(row.try_get::<i64, _>("format_version")?).map_err(
                    |error| JobError::internal(format!("invalid format version: {error}")),
                )?,
                backup_id: row.try_get("backup_id")?,
                created_at: row.try_get("created_at")?,
                source: serde_json::from_str(&row.try_get::<String, _>("source")?)?,
                compression_algorithm: row.try_get("compression")?,
                checksum_algorithm: row.try_get("checksum_algorithm")?,
                entries: Vec::new(),
            };
            manifest.validate_format()?;
            for row in sqlx::query("SELECT * FROM entries WHERE backup_id = ? ORDER BY ordinal")
                .bind(backup_id)
                .fetch_all(&mut *transaction)
                .await?
            {
                let path = serde_json::from_str(&row.try_get::<String, _>("path")?)?;
                let entry = match row.try_get::<String, _>("type")?.as_str() {
                    "directory" => ManifestEntry::Directory { path },
                    "file" => ManifestEntry::File {
                        path,
                        original_size: u64::try_from(row.try_get::<i64, _>("original_size")?)
                            .map_err(|error| {
                                JobError::internal(format!("invalid original size: {error}"))
                            })?,
                        checksum: row.try_get("checksum")?,
                    },
                    kind => return Err(JobError::internal(format!("invalid entry type {kind}"))),
                };
                manifest.entries.push(entry);
            }
            transaction.commit().await?;
            Ok::<_, JobError>(manifest)
        }
        .await;
        pool.close().await;
        result.map_err(|error| {
            JobError::new(
                error.code,
                format!(
                    "cannot read metadata for backup {backup_id} from {}: {error}",
                    path.display()
                ),
            )
        })
    }

    pub async fn write(&self, repository: &Path) -> Result<(), JobError> {
        self.validate_format()?;
        let path = repository.join("repository.sqlite3");
        let pool = storage::open(&path, REPO_SCHEMA, true).await?;
        let result = async {
            let mut transaction = pool.begin().await?;
            sqlx::query("INSERT INTO backups (backup_id, format_version, created_at, source, compression, checksum_algorithm) VALUES (?, ?, ?, ?, ?, ?)")
                .bind(&self.backup_id).bind(i64::from(self.format_version)).bind(&self.created_at)
                .bind(serde_json::to_string(&self.source)?).bind(&self.compression_algorithm).bind(&self.checksum_algorithm)
                .execute(&mut *transaction).await?;
            for (ordinal, entry) in self.entries.iter().enumerate() {
                let (kind, path, size, checksum) = match entry {
                    ManifestEntry::Directory { path } => ("directory", path, None, None),
                    ManifestEntry::File { path, original_size, checksum } => (
                        "file", path,
                        Some(i64::try_from(*original_size).map_err(|error| JobError::internal(format!("file size for {} is out of range: {error}", path.display())))?),
                        Some(checksum),
                    ),
                };
                let ordinal = i64::try_from(ordinal).map_err(|error| JobError::internal(format!("entry ordinal is out of range: {error}")))?;
                sqlx::query("INSERT INTO entries (backup_id, ordinal, type, path, original_size, checksum) VALUES (?, ?, ?, ?, ?, ?)")
                    .bind(&self.backup_id).bind(ordinal).bind(kind).bind(serde_json::to_string(path)?)
                    .bind(size).bind(checksum).execute(&mut *transaction).await?;
            }
            transaction.commit().await?;
            Ok::<_, JobError>(())
        }.await;
        pool.close().await;
        result.map_err(|error| {
            JobError::internal(format!(
                "cannot write metadata for backup {} to {}: {error}",
                self.backup_id,
                path.display()
            ))
        })
    }

    fn validate_format(&self) -> Result<(), JobError> {
        if self.format_version != 1
            || self.compression_algorithm != "zstd"
            || self.checksum_algorithm != "sha256"
        {
            return Err(JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "unsupported manifest format, compression or checksum algorithm",
            ));
        }
        Ok(())
    }
}

pub fn list(repository: &Path) -> Result<Vec<RepoBackupInfo>, JobError> {
    let absolute_repository_path = get_absolute_directory_path(repository)?;
    let absolute_db_path = absolute_repository_path.join("repository.sqlite3");

    if !absolute_db_path.try_exists().map_err(|error| {
        JobError::internal(format!(
            "failed to check whether repository.sqlite3 {} is existing or not: {error}",
            absolute_db_path.display()
        ))
    })? {
        return Ok(Vec::new());
    }

    let handle = tokio::runtime::Handle::try_current()
        .map_err(|error| JobError::internal(format!("cannot access Tokio runtime: {error}")))?;

    handle.block_on(async {
        let pool = storage::open(&absolute_db_path, REPO_SCHEMA, false).await?;

        let result = async {
            let rows = sqlx::query(
                "
                SELECT backup_id, created_at
                FROM backups
                ORDER BY created_at DESC, backup_id ASC
                ",
            )
            .fetch_all(&pool)
            .await?;

            let mut summaries = Vec::with_capacity(rows.len());
            for row in rows {
                summaries.push(RepoBackupInfo {
                    backup_id: row.try_get("backup_id")?,
                    created_at: row.try_get("created_at")?,
                });
            }
            Ok(summaries)
        }
        .await;

        pool.close().await;
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDirectory;
    use proptest::{
        collection::btree_map,
        prelude::*,
        test_runner::{Config, RngSeed, TestCaseError},
    };
    use std::{collections::BTreeMap, error::Error};

    type TestResult = Result<(), Box<dyn Error>>;

    #[tokio::test]
    async fn v2_schema_enforces_metadata_and_entry_types() -> TestResult {
        let directory = TestDirectory::new()?;
        let pool = storage::open(
            &directory.path().join("repository.sqlite3"),
            REPO_SCHEMA,
            true,
        )
        .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                .fetch_one(&pool)
                .await?,
            2
        );
        sqlx::query(
            "INSERT INTO backups (
            backup_id, format_version, created_at, source, compression, checksum_algorithm,
            root_uid, root_gid, root_mode, root_atime_seconds, root_atime_nanoseconds,
            root_mtime_seconds, root_mtime_nanoseconds,
            entries_created, skipped_unsupported, skipped_filtered
        ) VALUES ('backup', 2, '2026-10-23T00:00:00Z', '\"/source\"', 'zstd', 'sha256',
            4294967295, 0, 4095, -1, 999999999, 0, 0, 3, 2, 1)",
        )
        .execute(&pool)
        .await?;
        for (ordinal, kind, size, checksum, target) in [
            (0i64, "file", Some(0i64), Some("hash"), None),
            (1, "directory", None, None, None),
            (2, "symlink", None, None, Some("\"../missing\"")),
        ] {
            sqlx::query(
                "INSERT INTO entries (
                backup_id, ordinal, type, path, uid, gid, mode,
                atime_seconds, atime_nanoseconds, mtime_seconds, mtime_nanoseconds,
                original_size, checksum, link_target
            ) VALUES ('backup', ?, ?, ?, 1000, 1000, 420, 0, 0, 0, 0, ?, ?, ?)",
            )
            .bind(ordinal)
            .bind(kind)
            .bind(format!("\"{kind}\""))
            .bind(size)
            .bind(checksum)
            .bind(target)
            .execute(&pool)
            .await?;
        }
        for statement in [
            "UPDATE entries SET uid = -1",
            "UPDATE entries SET gid = 4294967296",
            "UPDATE entries SET mode = 4096",
            "UPDATE entries SET atime_nanoseconds = -1",
            "UPDATE entries SET mtime_nanoseconds = 1000000000",
            "UPDATE entries SET type = 'socket'",
            "UPDATE entries SET original_size = NULL WHERE type = 'file'",
            "UPDATE entries SET original_size = -1 WHERE type = 'file'",
            "UPDATE entries SET checksum = NULL WHERE type = 'file'",
            "UPDATE entries SET link_target = NULL WHERE type = 'symlink'",
            "UPDATE entries SET link_target = 'target' WHERE type = 'directory'",
            "UPDATE entries SET original_size = 0 WHERE type = 'symlink'",
            "UPDATE entries SET path = 'duplicate'",
            "UPDATE entries SET backup_id = 'missing'",
            "UPDATE backups SET format_version = 1",
            "UPDATE backups SET root_uid = NULL",
            "UPDATE backups SET root_mtime_nanoseconds = 1000000000",
            "UPDATE backups SET entries_created = -1",
            "UPDATE backups SET skipped_unsupported = -1",
            "UPDATE backups SET skipped_filtered = -1",
        ] {
            assert!(
                sqlx::query(statement).execute(&pool).await.is_err(),
                "accepted {statement}"
            );
        }
        pool.close().await;
        Ok(())
    }

    async fn list_at(
        repository: &Path,
    ) -> Result<Result<Vec<RepoBackupInfo>, JobError>, tokio::task::JoinError> {
        let repository = repository.to_owned();
        tokio::task::spawn_blocking(move || list(&repository)).await
    }

    fn header(repository: &Path, backup_id: &str, second: u8) -> Manifest {
        Manifest {
            format_version: 1,
            backup_id: backup_id.to_owned(),
            created_at: format!("2026-10-13T00:00:{second:02}.000Z"),
            source: repository.to_owned(),
            compression_algorithm: "zstd".into(),
            checksum_algorithm: "sha256".into(),
            entries: Vec::new(),
        }
    }

    async fn check_listing_order(backups: BTreeMap<String, u8>) -> TestResult {
        let directory = TestDirectory::new()?;
        let repository = directory.path();
        // Initialize even for an empty generated set, covering an empty metadata DB.
        storage::open(&repository.join("repository.sqlite3"), REPO_SCHEMA, true)
            .await?
            .close()
            .await;
        for (backup_id, second) in backups.iter().rev() {
            header(repository, backup_id, *second)
                .write(repository)
                .await?;
        }
        // The oracle sorts numeric times, independently of SQL/text ordering.
        let mut expected: Vec<_> = backups.into_iter().collect();
        expected.sort_by(|(left_id, left_time), (right_id, right_time)| {
            right_time
                .cmp(left_time)
                .then_with(|| left_id.cmp(right_id))
        });
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(id, second)| (id, format!("2026-10-13T00:00:{second:02}.000Z")))
            .collect();
        for _ in 0..2 {
            let actual: Vec<_> = list_at(repository)
                .await??
                .into_iter()
                .map(|summary| (summary.backup_id, summary.created_at))
                .collect();
            assert_eq!(actual, expected);
        }
        Ok(())
    }

    proptest! {
        #![proptest_config(Config {
            cases: 32,
            rng_seed: RngSeed::Fixed(0xBAC1_2026),
            ..Config::default()
        })]

        #[test]
        fn generated_headers_are_listed_in_stable_order(
            backups in btree_map("[a-z][a-z0-9]{0,7}", 0u8..4, 0..10),
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all()
                .build().map_err(|error| TestCaseError::fail(error.to_string()))?;
            runtime.block_on(check_listing_order(backups))
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
        }
    }

    #[tokio::test]
    async fn newest_first_and_equal_timestamp_ids_ascending() -> TestResult {
        check_listing_order(
            [
                ("older".into(), 0),
                ("z-newest".into(), 3),
                ("a-newest".into(), 3),
            ]
            .into(),
        )
        .await
    }

    #[tokio::test]
    async fn unpublished_directories_do_not_initialize_metadata() -> TestResult {
        let directory = TestDirectory::new()?;
        let repository = directory.path();
        assert!(list_at(repository).await??.is_empty());
        std::fs::create_dir_all(repository.join("backups/unpublished"))?;
        std::fs::write(
            repository.join("backups/unpublished/payload"),
            b"keep contents",
        )?;
        assert!(list_at(repository).await??.is_empty());
        assert!(!repository.join("repository.sqlite3").exists());
        assert_eq!(std::fs::read_dir(repository)?.count(), 1);
        assert_eq!(
            std::fs::read(repository.join("backups/unpublished/payload"))?,
            b"keep contents"
        );
        Ok(())
    }

    #[tokio::test]
    async fn failed_publication_does_not_appear_in_listing() -> TestResult {
        let directory = TestDirectory::new()?;
        let repository = directory.path();
        header(repository, "published", 0).write(repository).await?;
        std::fs::create_dir_all(repository.join("backups/unpublished"))?;
        let mut failed = header(repository, "unpublished", 3);
        failed.entries.push(ManifestEntry::File {
            path: "out-of-range".into(),
            original_size: u64::MAX,
            checksum: "hash".into(),
        });
        assert!(failed.write(repository).await.is_err());
        let summaries = list_at(repository).await??;
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].backup_id, "published");
        assert_eq!(summaries[0].created_at, "2026-10-13T00:00:00.000Z");
        assert!(repository.join("backups/unpublished").is_dir());
        Ok(())
    }

    #[tokio::test]
    async fn invalid_metadata_is_an_error_instead_of_an_empty_list() -> TestResult {
        for damage in ["corrupt", "unsupported-schema", "missing-table"] {
            let directory = TestDirectory::new()?;
            let repository = directory.path();
            let database = repository.join("repository.sqlite3");
            if damage == "corrupt" {
                std::fs::write(&database, b"invalid SQLite database")?;
            } else {
                let pool = storage::open(&database, REPO_SCHEMA, true).await?;
                if damage == "unsupported-schema" {
                    sqlx::query("PRAGMA user_version = 99")
                        .execute(&pool)
                        .await?;
                } else {
                    sqlx::query("DROP TABLE backups").execute(&pool).await?;
                }
                pool.close().await;
            }
            let error = list_at(repository)
                .await?
                .err()
                .ok_or("accepted invalid metadata")?;
            assert_eq!(error.code, StatusCode::INTERNAL_SERVER_ERROR);
            assert!(!error.message.is_empty());
            assert!(database.exists());
        }
        Ok(())
    }

    #[tokio::test]
    async fn invalid_repository_paths_are_rejected() -> TestResult {
        let directory = TestDirectory::new()?;
        let file = directory.path().join("file");
        let missing = directory.path().join("missing");
        std::fs::write(&file, b"keep contents")?;
        let error = list_at(&file)
            .await?
            .err()
            .ok_or("accepted file as repository")?;
        assert_eq!(error.code, StatusCode::BAD_REQUEST);
        let error = list_at(&missing)
            .await?
            .err()
            .ok_or("accepted missing repository")?;
        assert_eq!(error.code, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!missing.exists());
        assert_eq!(std::fs::read(&file)?, b"keep contents");
        Ok(())
    }

    fn manifest() -> Manifest {
        Manifest {
            format_version: 1,
            backup_id: "backup-001".into(),
            created_at: "2026-10-09T00:00:00Z".into(),
            source: "/source".into(),
            compression_algorithm: "zstd".into(),
            checksum_algorithm: "sha256".into(),
            entries: vec![
                ManifestEntry::Directory {
                    path: "empty".into(),
                },
                ManifestEntry::File {
                    path: "file.txt".into(),
                    original_size: 0,
                    checksum: "empty-file-sha256".into(),
                },
            ],
        }
    }

    #[tokio::test]
    async fn metadata_round_trip_and_duplicate_ids() -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let repository = directory.path();
        assert!(Manifest::read(repository, "backup-001").await.is_err());
        assert!(!repository.join("repository.sqlite3").exists());
        let manifest = manifest();
        manifest.write(repository).await?;
        let expected = serde_json::to_value(&manifest)?;
        assert_eq!(
            serde_json::to_value(Manifest::read(repository, &manifest.backup_id).await?)?,
            expected
        );
        assert!(manifest.write(repository).await.is_err());
        assert_eq!(
            serde_json::to_value(Manifest::read(repository, &manifest.backup_id).await?)?,
            expected
        );
        let error = Manifest::read(repository, "missing")
            .await
            .err()
            .ok_or("accepted missing backup")?;
        assert_eq!(error.code, StatusCode::NOT_FOUND);
        assert!(!repository.join("manifest.json").exists());
        Ok(())
    }

    #[tokio::test]
    async fn failed_metadata_writes_roll_back_header_and_entries()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let mut manifest = manifest();
        manifest.entries.push(ManifestEntry::File {
            path: "too-large".into(),
            original_size: u64::MAX,
            checksum: "hash".into(),
        });
        let error = manifest
            .write(directory.path())
            .await
            .err()
            .ok_or("accepted out-of-range file size")?;
        assert!(error.message.contains("too-large"));
        let pool = storage::open(
            &directory.path().join("repository.sqlite3"),
            REPO_SCHEMA,
            false,
        )
        .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM backups")
                .fetch_one(&pool)
                .await?,
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM entries")
                .fetch_one(&pool)
                .await?,
            0
        );
        pool.close().await;
        manifest.entries.pop();
        manifest.write(directory.path()).await?;
        manifest.format_version = 2;
        assert!(manifest.write(directory.path()).await.is_err());
        let pool = storage::open(
            &directory.path().join("repository.sqlite3"),
            REPO_SCHEMA,
            false,
        )
        .await?;
        sqlx::query("UPDATE backups SET compression = 'unknown'")
            .execute(&pool)
            .await?;
        pool.close().await;
        assert!(
            Manifest::read(directory.path(), &manifest.backup_id)
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn legacy_manifests_are_not_imported() -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let legacy = serde_json::to_vec(&manifest())?;
        std::fs::write(directory.path().join("manifest.json"), &legacy)?;
        assert!(
            Manifest::read(directory.path(), "backup-001")
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(directory.path().join("manifest.json"))?,
            legacy
        );
        Ok(())
    }
}
