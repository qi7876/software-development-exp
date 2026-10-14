use super::{JobError, filesystem::get_absolute_directory_path, storage};
use http::StatusCode;
use sqlx::Row;
use std::path::{Path, PathBuf};

#[derive(serde::Serialize)]
pub struct RepoBackupInfo {
    pub backup_id: String,
    pub created_at: String,
}

pub const REPO_SCHEMA: &str = "
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
