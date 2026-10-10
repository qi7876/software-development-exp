use http::StatusCode;
use sqlx::Row;
use std::path::{Path, PathBuf};

use crate::{jobs::JobError, storage};

#[derive(serde::Serialize)]
pub(crate) struct BackupSummary {
    pub(crate) backup_id: String,
    pub(crate) created_at: String,
}

pub(crate) fn list(_repository: &Path) -> Result<Vec<BackupSummary>, JobError> {
    Err(JobError::new(
        StatusCode::NOT_IMPLEMENTED,
        "backup listing is not implemented yet",
    ))
}

pub(crate) fn run(_source: &Path, _repository: &Path, _backup_id: &str) -> Result<(), JobError> {
    Err(JobError::new(
        StatusCode::NOT_IMPLEMENTED,
        "backup execution is not implemented yet",
    ))
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "Metadata IO will be called by backup and restore")
)]
const SCHEMA: &str = "
CREATE TABLE backups (
    backup_id TEXT PRIMARY KEY NOT NULL,
    format_version INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    source TEXT NOT NULL,
    compression TEXT NOT NULL,
    checksum_algorithm TEXT NOT NULL
);
CREATE TABLE entries (
    backup_id TEXT NOT NULL REFERENCES backups(backup_id),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    type TEXT NOT NULL CHECK (type IN ('file', 'directory')),
    path TEXT NOT NULL,
    original_size INTEGER,
    checksum TEXT,
    PRIMARY KEY (backup_id, ordinal),
    CHECK (
        (type = 'file' AND original_size IS NOT NULL AND original_size >= 0 AND checksum IS NOT NULL) OR
        (type = 'directory' AND original_size IS NULL AND checksum IS NULL)
    )
);
";

// Repository metadata is committed once and is immutable thereafter.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "Manifest writing will be wired into backup::run")
)]
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Manifest {
    pub(crate) format_version: u32,
    pub(crate) backup_id: String,
    pub(crate) created_at: String,
    pub(crate) source: PathBuf,
    pub(crate) compression: String,
    pub(crate) checksum_algorithm: String,
    pub(crate) entries: Vec<ManifestEntry>,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Manifest entries will be collected by backup::run"
    )
)]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ManifestEntry {
    File {
        path: PathBuf,
        original_size: u64,
        checksum: String,
    },
    Directory {
        path: PathBuf,
    },
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "Manifest IO will be called by backup and restore")
)]
impl Manifest {
    pub(crate) async fn read(repository: &Path, backup_id: &str) -> Result<Self, JobError> {
        let path = repository.join("repository.sqlite3");
        let pool = storage::open(&path, SCHEMA, false).await?;
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
                compression: row.try_get("compression")?,
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

    pub(crate) async fn write(&self, repository: &Path) -> Result<(), JobError> {
        self.validate_format()?;
        let path = repository.join("repository.sqlite3");
        let pool = storage::open(&path, SCHEMA, true).await?;
        let result = async {
            let mut transaction = pool.begin().await?;
            sqlx::query("INSERT INTO backups (backup_id, format_version, created_at, source, compression, checksum_algorithm) VALUES (?, ?, ?, ?, ?, ?)")
                .bind(&self.backup_id).bind(i64::from(self.format_version)).bind(&self.created_at)
                .bind(serde_json::to_string(&self.source)?).bind(&self.compression).bind(&self.checksum_algorithm)
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
            || self.compression != "zstd"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDirectory;

    fn manifest() -> Manifest {
        Manifest {
            format_version: 1,
            backup_id: "backup-001".into(),
            created_at: "2026-10-09T00:00:00Z".into(),
            source: "/source".into(),
            compression: "zstd".into(),
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
        let pool =
            storage::open(&directory.path().join("repository.sqlite3"), SCHEMA, false).await?;
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
        let pool =
            storage::open(&directory.path().join("repository.sqlite3"), SCHEMA, false).await?;
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
