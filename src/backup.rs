use chrono::{SecondsFormat, Utc};
use http::StatusCode;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use crate::{jobs::JobError, storage};

#[derive(serde::Serialize)]
pub(crate) struct BackupSummary {
    pub(crate) backup_id: String,
    pub(crate) created_at: String,
}

#[derive(Debug, PartialEq, Eq)]
enum BackupEntry {
    File(PathBuf),
    Directory(PathBuf),
}

struct FileBackupResult {
    original_size: u64,
    checksum: String,
}

pub fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

pub fn get_absolute_directory_path(path: &Path) -> Result<PathBuf, JobError> {
    let path = std::fs::canonicalize(path).map_err(|error| {
        JobError::internal(format!(
            "failed to resolve path {}: {error}",
            path.display()
        ))
    })?;
    let path_metadata = path.metadata().map_err(|error| {
        JobError::internal(format!(
            "failed to load path {} metadata : {error}",
            path.display(),
        ))
    })?;
    if !path_metadata.is_dir() {
        return Err(JobError::new(
            StatusCode::BAD_REQUEST,
            format!("path {} is not a directory", path.display()),
        ));
    };
    Ok(path)
}

pub(crate) fn list(repository: &Path) -> Result<Vec<BackupSummary>, JobError> {
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
        let pool = storage::open(&absolute_db_path, SCHEMA, false).await?;

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
                summaries.push(BackupSummary {
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

fn walk_directory(
    absolute_root: &Path,
    absolute_current: &Path,
    backup_entries: &mut Vec<BackupEntry>,
) -> Result<(), JobError> {
    let source_dir = std::fs::read_dir(absolute_current).map_err(|error| {
        JobError::internal(format!(
            "failed to open path {}: {error}",
            absolute_current.display(),
        ))
    })?;
    for file in source_dir {
        let file = file.map_err(|error| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "failed to get file under path {}: {error}",
                    absolute_current.display(),
                ),
            )
        })?;
        let absolute_file_path = file.path();
        let relative_file_path = absolute_file_path
            .strip_prefix(absolute_root)
            .map_err(|error| {
                JobError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "failed to strip file prefix {}: {error}",
                        file.path().display(),
                    ),
                )
            })?
            .to_path_buf();
        let file_type = file.file_type().map_err(|error| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "failed to get file type {}: {error}",
                    relative_file_path.display(),
                ),
            )
        })?;
        if file_type.is_dir() {
            backup_entries.push(BackupEntry::Directory(relative_file_path));
            walk_directory(absolute_root, &absolute_file_path, backup_entries)?;
        } else if file_type.is_file() {
            backup_entries.push(BackupEntry::File(relative_file_path));
        } else if file_type.is_symlink() {
            return Err(JobError::new(
                StatusCode::BAD_REQUEST,
                format!("find symlink file: {}", relative_file_path.display()),
            ));
        } else {
            return Err(JobError::new(
                StatusCode::BAD_REQUEST,
                format!("unsupported file: {}", relative_file_path.display()),
            ));
        };
    }
    Ok(())
}

/// Returns paths relative to the source directory.
fn path_traversal(absolute_source_path: &Path) -> Result<Vec<BackupEntry>, JobError> {
    let mut backup_entries = Vec::<BackupEntry>::new();
    walk_directory(
        absolute_source_path,
        absolute_source_path,
        &mut backup_entries,
    )?;
    Ok(backup_entries)
}

fn backup_one_file(
    absolute_source_file_path: &Path,
    absolute_backup_file_path: &Path,
) -> Result<FileBackupResult, JobError> {
    let mut source_file = std::fs::OpenOptions::new()
        .read(true)
        .open(absolute_source_file_path)
        .map_err(|error| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "failed to open source file {}: {error}",
                    absolute_source_file_path.display(),
                ),
            )
        })?;
    let backup_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(absolute_backup_file_path)
        .map_err(|error| match error.kind() {
            io::ErrorKind::AlreadyExists => JobError::new(
                StatusCode::CONFLICT,
                format!(
                    "backup file {} already exists: {error}",
                    absolute_backup_file_path.display(),
                ),
            ),
            _ => JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "failed to create backup file {}: {error}",
                    absolute_backup_file_path.display(),
                ),
            ),
        })?;

    let mut buffer = [0u8; 128 * 1024];
    let mut backup_file_encoder =
        zstd::stream::write::Encoder::new(backup_file, 0).map_err(|error| {
            JobError::internal(format!(
                "failed to create zstd encoder on backup file {}: {error}",
                absolute_backup_file_path.display(),
            ))
        })?;
    let mut hasher = Sha256::new();
    let mut original_size: u64 = 0;

    loop {
        let n = source_file.read(&mut buffer).map_err(|error| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "failed to read source file {}: {error}",
                    absolute_source_file_path.display(),
                ),
            )
        })?;

        if n == 0 {
            break;
        }

        let chunk = &buffer[..n];
        backup_file_encoder.write_all(chunk).map_err(|error| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "failed to compress and forward data to backup file {}: {error}",
                    absolute_backup_file_path.display(),
                ),
            )
        })?;
        hasher.update(chunk);
        original_size += n as u64;
    }
    let backup_file = backup_file_encoder.finish().map_err(|error| {
        JobError::internal(format!(
            "failed to finish writing on backup file {}: {error}",
            absolute_backup_file_path.display(),
        ))
    })?;

    backup_file.sync_all().map_err(|error| {
        JobError::internal(format!(
            "failed to sync backup file {}: {error}",
            absolute_backup_file_path.display(),
        ))
    })?;

    Ok(FileBackupResult {
        original_size,
        checksum: hex::encode(hasher.finalize()),
    })
}

#[cfg(unix)]
pub(crate) fn sync_directory(path: &Path) -> Result<(), JobError> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            JobError::internal(format!(
                "failed to sync directory {}: {error}",
                path.display()
            ))
        })
}

#[cfg(not(unix))]
pub(crate) fn sync_directory(path: &Path) -> Result<(), JobError> {
    Err(JobError::new(
        StatusCode::NOT_IMPLEMENTED,
        format!(
            "directory synchronization is not implemented on this platform: {}",
            path.display()
        ),
    ))
}

fn backup_all_files(
    backup_entries: &Vec<BackupEntry>,
    absolute_source_path: &Path,
    absolute_backup_root_directory_path: &Path,
) -> Result<Vec<ManifestEntry>, JobError> {
    let mut manifest_entries = Vec::<ManifestEntry>::new();
    for backup_entry in backup_entries {
        let manifest_entry = match backup_entry {
            // The parent directory is always created before writing files.
            BackupEntry::File(relative_path) => {
                let absolute_source_file_path = absolute_source_path.join(relative_path);
                let absolute_backup_file_path =
                    absolute_backup_root_directory_path.join(relative_path);
                let backup_result =
                    backup_one_file(&absolute_source_file_path, &absolute_backup_file_path)?;
                ManifestEntry::File {
                    path: relative_path.to_path_buf(),
                    original_size: backup_result.original_size,
                    checksum: backup_result.checksum,
                }
            }
            BackupEntry::Directory(relative_path) => {
                let absolute_backup_directory_path =
                    absolute_backup_root_directory_path.join(relative_path);
                std::fs::create_dir(&absolute_backup_directory_path).map_err(|error| {
                    JobError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!(
                            "failed to create new backup directory {}: {error}",
                            absolute_backup_directory_path.display(),
                        ),
                    )
                })?;
                ManifestEntry::Directory {
                    path: relative_path.to_path_buf(),
                }
            }
        };
        manifest_entries.push(manifest_entry);
    }

    for manifest_entry in manifest_entries.iter().rev() {
        if let ManifestEntry::Directory { path } = manifest_entry {
            sync_directory(&absolute_backup_root_directory_path.join(path))?;
        }
    }

    Ok(manifest_entries)
}

pub(crate) fn run(source: &Path, repository: &Path, backup_id: &str) -> Result<(), JobError> {
    let absolute_source_path = get_absolute_directory_path(source)?;
    let absolute_repository_path = get_absolute_directory_path(repository)?;
    if paths_overlap(&absolute_source_path, &absolute_repository_path) {
        return Err(JobError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "source path {} and repository path {} overlap",
                absolute_source_path.display(),
                absolute_repository_path.display()
            ),
        ));
    }
    let backup_entries = path_traversal(&absolute_source_path)?;

    #[cfg(not(unix))]
    sync_directory(&absolute_repository_path)?;

    std::fs::create_dir_all(absolute_repository_path.join("backups")).map_err(|error| {
        JobError::internal(format!(
            "failed to create backups directory {}: {error}",
            absolute_repository_path.join("backups").display(),
        ))
    })?;

    let absolute_backup_root_directory_path =
        absolute_repository_path.join(format!("backups/{}", backup_id));

    std::fs::create_dir(&absolute_backup_root_directory_path).map_err(|error| {
        match error.kind() {
            io::ErrorKind::AlreadyExists => JobError::new(
                StatusCode::CONFLICT,
                format!("backup id {backup_id} already exists: {error}"),
            ),
            _ => JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "failed to create backup id directory {}: {error}",
                    absolute_backup_root_directory_path.display(),
                ),
            ),
        }
    })?;

    let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let manifest_entries = backup_all_files(
        &backup_entries,
        &absolute_source_path,
        &absolute_backup_root_directory_path,
    )?;

    sync_directory(&absolute_backup_root_directory_path)?;
    sync_directory(&absolute_repository_path.join("backups"))?;
    sync_directory(&absolute_repository_path)?;

    let manifest = Manifest {
        format_version: 1,
        backup_id: backup_id.to_owned(),
        created_at,
        source: absolute_source_path,
        compression_algorithm: "zstd".to_string(),
        checksum_algorithm: "sha256".to_string(),
        entries: manifest_entries,
    };
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|error| JobError::internal(format!("cannot access Tokio runtime: {error}")))?;
    handle.block_on(manifest.write(&absolute_repository_path))?;

    Ok(())
}

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
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Manifest {
    pub(crate) format_version: u32,
    pub(crate) backup_id: String,
    pub(crate) created_at: String,
    pub(crate) source: PathBuf,
    pub(crate) compression_algorithm: String,
    pub(crate) checksum_algorithm: String,
    pub(crate) entries: Vec<ManifestEntry>,
}

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

    pub(crate) async fn write(&self, repository: &Path) -> Result<(), JobError> {
        self.validate_format()?;
        let path = repository.join("repository.sqlite3");
        let pool = storage::open(&path, SCHEMA, true).await?;
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

#[cfg(test)]
mod list_tests {
    // Exercise listing against real repository metadata without relying on file IO order.
    use super::*;
    use crate::test_support::TestDirectory;
    use proptest::{
        collection::btree_map,
        prelude::*,
        test_runner::{Config, RngSeed, TestCaseError},
    };
    use std::{collections::BTreeMap, error::Error};

    type TestResult = Result<(), Box<dyn Error>>;

    async fn list_at(
        repository: &Path,
    ) -> Result<Result<Vec<BackupSummary>, JobError>, tokio::task::JoinError> {
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
        storage::open(&repository.join("repository.sqlite3"), SCHEMA, true)
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
                let pool = storage::open(&database, SCHEMA, true).await?;
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
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;
    use crate::test_support::TestDirectory;

    #[test]
    fn overlap_paths() {
        let source_1 = Path::new("/data/source");
        let backup_1 = Path::new("/data/backup");
        assert!(!paths_overlap(source_1, backup_1));
        assert!(paths_overlap(source_1, source_1));
        let backup_2 = Path::new("/data/source/backup");
        assert!(paths_overlap(source_1, backup_2));
        let source_2 = Path::new("/data/backup/source");
        assert!(paths_overlap(source_2, backup_1));
        let backup_3 = Path::new("/data/source-copy");
        assert!(!paths_overlap(source_1, backup_3));
    }

    #[test]
    fn send_file_path_to_run() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let repo = directory.path().join("repo");
        std::fs::write(&source, "hello")?;
        std::fs::create_dir(&repo)?;
        let backup_id = "12345";
        let err = run(&source, &repo, backup_id)
            .err()
            .ok_or("accepted a file as source")?;
        assert_eq!(err.code, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("not a directory"));
        Ok(())
    }

    #[test]
    fn send_overlaped_paths_to_run() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let repo = directory.path().join("source/repo");
        std::fs::create_dir(&source)?;
        std::fs::create_dir(&repo)?;
        let backup_id = "12345";
        let err = run(&source, &repo, backup_id)
            .err()
            .ok_or("accepted overlaped paths")?;
        assert_eq!(err.code, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("overlap"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn send_source_alias_to_run() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let alias = directory.path().join("alias");
        std::fs::create_dir(&source)?;
        std::os::unix::fs::symlink(&source, &alias)?;
        let backup_id = "12345";
        let err = run(&source, &alias, backup_id)
            .err()
            .ok_or("accepted overlaped symlink paths")?;
        assert_eq!(err.code, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("overlap"));
        Ok(())
    }

    #[test]
    fn simple_traversal() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let source_file = directory.path().join("source/hello.txt");
        let source_empty = directory.path().join("source/empty");
        std::fs::create_dir(&source)?;
        std::fs::write(&source_file, "hello")?;
        std::fs::create_dir(&source_empty)?;
        let entries = path_traversal(&source)?;
        assert_eq!(entries.len(), 2);
        assert!(entries.contains(&BackupEntry::File(PathBuf::from("hello.txt"))));
        assert!(entries.contains(&BackupEntry::Directory(PathBuf::from("empty"))));
        Ok(())
    }

    #[test]
    fn recursive_traversal() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let source_file = directory.path().join("source/hello.txt");
        let source_docs = directory.path().join("source/docs");
        let source_docs_file = directory.path().join("source/docs/README.txt");
        let source_empty = directory.path().join("source/empty");
        std::fs::create_dir(&source)?;
        std::fs::write(&source_file, "hello")?;
        std::fs::create_dir(&source_docs)?;
        std::fs::write(&source_docs_file, "readme")?;
        std::fs::create_dir(&source_empty)?;
        let entries = path_traversal(&source)?;
        assert_eq!(entries.len(), 4);
        assert!(entries.contains(&BackupEntry::File(PathBuf::from("hello.txt"))));
        assert!(entries.contains(&BackupEntry::Directory(PathBuf::from("empty"))));
        assert!(entries.contains(&BackupEntry::Directory(PathBuf::from("docs"))));
        assert!(entries.contains(&BackupEntry::File(PathBuf::from("docs/README.txt"))));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recursive_traversal_reject_symlinks() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let source_file = directory.path().join("source/hello.txt");
        let source_docs = directory.path().join("source/docs");
        let alias = directory.path().join("source/docs/alias");
        std::fs::create_dir(&source)?;
        std::fs::write(&source_file, "hello")?;
        std::fs::create_dir(&source_docs)?;
        std::os::unix::fs::symlink(&source_file, &alias)?;
        let err = path_traversal(&source)
            .err()
            .ok_or("accepted symlink file")?;
        assert_eq!(err.code, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("symlink"));
        assert!(err.message.contains("docs/alias"));
        Ok(())
    }

    #[test]
    fn backup_file_exist() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let source_file = directory.path().join("source/hello.txt");
        let backup = directory.path().join("backup");
        let backup_file = directory.path().join("backup/hello.txt");
        std::fs::create_dir(&source)?;
        std::fs::write(&source_file, "hello")?;
        std::fs::create_dir(&backup)?;
        std::fs::write(&backup_file, "existing backup")?;
        let err = backup_one_file(&source_file, &backup_file)
            .err()
            .ok_or("write to an existing file")?;
        assert_eq!(std::fs::read_to_string(&backup_file)?, "existing backup");
        assert_eq!(err.code, StatusCode::CONFLICT);
        assert!(err.message.contains("exists"));
        Ok(())
    }

    #[test]
    fn simple_backup_one_file() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let source_file = directory.path().join("source/hello.txt");
        let backup = directory.path().join("backup");
        let backup_file = directory.path().join("backup/hello.txt");

        std::fs::create_dir(&source)?;
        std::fs::write(&source_file, "hello")?;
        std::fs::create_dir(&backup)?;

        let result = backup_one_file(&source_file, &backup_file)?;
        assert_eq!(result.original_size, 5);
        let checksum = hex::encode(Sha256::digest(std::fs::read(source_file)?));
        assert_eq!(checksum, result.checksum);
        let decompressed = zstd::decode_all(std::fs::File::open(backup_file)?)?;
        assert_eq!(std::str::from_utf8(&decompressed)?, "hello");
        Ok(())
    }

    #[test]
    fn simple_backup_empty_file() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let source_file = directory.path().join("source/hello.txt");
        let backup = directory.path().join("backup");
        let backup_file = directory.path().join("backup/hello.txt");

        std::fs::create_dir(&source)?;
        std::fs::write(&source_file, "")?;
        std::fs::create_dir(&backup)?;
        let result = backup_one_file(&source_file, &backup_file)?;

        let checksum = hex::encode(Sha256::digest(std::fs::read(source_file)?));
        let decompressed = zstd::decode_all(std::fs::File::open(backup_file)?)?;
        assert_eq!(result.original_size, 0);
        assert_eq!(checksum, result.checksum);
        assert_eq!(std::str::from_utf8(&decompressed)?, "");

        Ok(())
    }

    #[test]
    fn backup_binary_file_across_multiple_chunks() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source.bin");
        let backup = directory.path().join("backup.zst");

        let content: Vec<u8> = (0u8..=255).cycle().take(300_001).collect();

        std::fs::write(&source, &content)?;
        let result = backup_one_file(&source, &backup)?;

        let decompressed = zstd::decode_all(std::fs::File::open(&backup)?)?;
        assert_eq!(decompressed, content);
        assert_eq!(result.original_size, 300_001);
        assert_eq!(result.checksum, hex::encode(Sha256::digest(&content)));
        assert_eq!(std::fs::read(&source)?, content);

        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn full_backup_run() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let repository = directory.path().join("repository");
        std::fs::create_dir(&source)?;
        std::fs::create_dir(&repository)?;

        std::fs::create_dir(source.join("dir1"))?;
        std::fs::create_dir(source.join("empty"))?;
        let files: [(&str, &[u8]); 2] =
            [("hello.txt", b"hello"), ("dir1/hello.txt", b"nested hello")];
        for (path, content) in files {
            std::fs::write(source.join(path), content)?;
        }
        assert!(!repository.join("backups").exists());

        let source_for_run = source.clone();
        let repository_for_run = repository.clone();

        tokio::task::spawn_blocking(move || {
            run(&source_for_run, &repository_for_run, "backup-001")
        })
        .await??;

        let manifest = Manifest::read(&repository, "backup-001").await?;
        assert_eq!(manifest.backup_id, "backup-001");
        assert_eq!(manifest.source, std::fs::canonicalize(&source)?);
        assert_eq!(manifest.format_version, 1);
        assert_eq!(manifest.compression_algorithm, "zstd");
        assert_eq!(manifest.checksum_algorithm, "sha256");
        let created_at = chrono::DateTime::parse_from_rfc3339(&manifest.created_at)?;
        assert_eq!(created_at.offset().local_minus_utc(), 0);
        assert_eq!(manifest.entries.len(), 4);

        let backup_root = repository.join("backups/backup-001");
        for (relative_path, content) in files {
            let (original_size, checksum) = manifest
                .entries
                .iter()
                .find_map(|entry| match entry {
                    ManifestEntry::File {
                        path,
                        original_size,
                        checksum,
                    } if path.as_path() == Path::new(relative_path) => {
                        Some((*original_size, checksum))
                    }
                    _ => None,
                })
                .ok_or_else(|| format!("missing file entry {relative_path}"))?;
            assert_eq!(original_size, content.len() as u64);
            assert_eq!(checksum, &hex::encode(Sha256::digest(content)));
            let decompressed =
                zstd::decode_all(std::fs::File::open(backup_root.join(relative_path))?)?;
            assert_eq!(decompressed, content);
            assert_eq!(std::fs::read(source.join(relative_path))?, content);
        }
        for relative_path in ["dir1", "empty"] {
            assert!(manifest.entries.iter().any(|entry| matches!(
                entry,
                ManifestEntry::Directory { path }
                    if path.as_path() == Path::new(relative_path)
            )));
            assert!(backup_root.join(relative_path).is_dir());
            assert!(source.join(relative_path).is_dir());
        }
        assert!(
            std::fs::read_dir(backup_root.join("empty"))?
                .next()
                .transpose()?
                .is_none()
        );
        assert!(
            std::fs::read_dir(source.join("empty"))?
                .next()
                .transpose()?
                .is_none()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn full_backup_run_rejects_existing_backup_id() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let repository = directory.path().join("repository");
        std::fs::create_dir(&source)?;
        std::fs::create_dir(&repository)?;
        let source_file = source.join("hello.txt");
        std::fs::write(&source_file, b"original")?;

        let source_for_run = source.clone();
        let repository_for_run = repository.clone();
        tokio::task::spawn_blocking(move || {
            run(&source_for_run, &repository_for_run, "backup-001")
        })
        .await??;
        let expected_manifest =
            serde_json::to_value(Manifest::read(&repository, "backup-001").await?)?;
        let backup_file = repository.join("backups/backup-001/hello.txt");
        let expected_compressed = std::fs::read(&backup_file)?;
        std::fs::write(&source_file, b"changed")?;

        let source_for_retry = source.clone();
        let repository_for_retry = repository.clone();
        let retry = tokio::task::spawn_blocking(move || {
            run(&source_for_retry, &repository_for_retry, "backup-001")
        })
        .await?;
        let error = retry.err().ok_or("accepted duplicate backup ID")?;
        assert_eq!(error.code, StatusCode::CONFLICT);
        assert_eq!(
            serde_json::to_value(Manifest::read(&repository, "backup-001").await?)?,
            expected_manifest
        );
        assert_eq!(std::fs::read(&backup_file)?, expected_compressed);
        assert_eq!(
            zstd::decode_all(std::fs::File::open(&backup_file)?)?,
            b"original"
        );
        assert_eq!(std::fs::read(&source_file)?, b"changed");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn full_backup_run_does_not_publish_failed_metadata() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let repository = directory.path().join("repository");
        std::fs::create_dir(&source)?;
        std::fs::create_dir(&repository)?;
        let source_file = source.join("hello.txt");
        std::fs::write(&source_file, b"original")?;

        let source_for_run = source.clone();
        let repository_for_run = repository.clone();
        tokio::task::spawn_blocking(move || {
            run(&source_for_run, &repository_for_run, "backup-001")
        })
        .await??;
        let expected_manifest =
            serde_json::to_value(Manifest::read(&repository, "backup-001").await?)?;
        let existing_backup_file = repository.join("backups/backup-001/hello.txt");
        let expected_compressed = std::fs::read(&existing_backup_file)?;

        let database_path = repository.join("repository.sqlite3");
        let pool = storage::open(&database_path, SCHEMA, false).await?;
        // Fail after inserting the header, exercising rollback and propagation
        // through run with a real SQLite transaction.
        sqlx::query(
            "CREATE TRIGGER reject_backup_entry BEFORE INSERT ON entries
             WHEN NEW.backup_id = 'backup-002'
             BEGIN SELECT RAISE(ABORT, 'injected metadata publication failure'); END",
        )
        .execute(&pool)
        .await?;
        pool.close().await;
        std::fs::write(&source_file, b"changed")?;

        let source_for_run = source.clone();
        let repository_for_run = repository.clone();
        let result = tokio::task::spawn_blocking(move || {
            run(&source_for_run, &repository_for_run, "backup-002")
        })
        .await?;
        let error = result.err().ok_or("accepted failed metadata publication")?;
        assert_eq!(error.code, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            error
                .message
                .contains("injected metadata publication failure")
        );

        // The contents were written before publication was attempted. Their
        // presence alone must not make the backup visible in repository metadata.
        assert_eq!(
            zstd::decode_all(std::fs::File::open(
                repository.join("backups/backup-002/hello.txt")
            )?)?,
            b"changed"
        );
        let missing = Manifest::read(&repository, "backup-002")
            .await
            .err()
            .ok_or("published failed backup")?;
        assert_eq!(missing.code, StatusCode::NOT_FOUND);
        let pool = storage::open(&database_path, SCHEMA, false).await?;
        for query in [
            "SELECT COUNT(*) FROM backups",
            "SELECT COUNT(*) FROM entries",
        ] {
            assert_eq!(
                sqlx::query_scalar::<_, i64>(query).fetch_one(&pool).await?,
                1
            );
        }
        pool.close().await;
        assert_eq!(
            serde_json::to_value(Manifest::read(&repository, "backup-001").await?)?,
            expected_manifest
        );
        assert_eq!(std::fs::read(&existing_backup_file)?, expected_compressed);
        assert_eq!(std::fs::read(&source_file)?, b"changed");
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
