use super::{
    JobError,
    content::{BackupOptions, Content},
    crypto::Keyring,
    filesystem::{get_absolute_directory_path, paths_overlap, sync_directory},
    filter::{CompiledFilter, Filter},
    repository::{Manifest, ManifestEntry, ManifestEntryKind, UnixMetadata},
};
use chrono::{SecondsFormat, Utc};
use http::StatusCode;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

struct BackupEntry {
    relative_path: PathBuf,
    metadata: UnixMetadata,
    kind: BackupEntryKind,
}

enum BackupEntryKind {
    File,
    Directory,
    Symlink,
}

struct FileBackupResult {
    original_size: u64,
    checksum: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct BackupStats {
    pub(super) entries_created: i64,
    pub(super) skipped_unsupported: i64,
    pub(super) skipped_filtered: i64,
}

fn walk_directory(
    absolute_root: &Path,
    absolute_current: &Path,
    compiled_filter: &CompiledFilter,
    backip_stats: &mut BackupStats,
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
        if !file_type.is_dir() && !file_type.is_file() && !file_type.is_symlink() {
            backip_stats.skipped_unsupported += 1;
            continue;
        }

        let metadata = UnixMetadata::read(&absolute_file_path)?;
        if !compiled_filter.matches(&relative_file_path, &metadata)? {
            backip_stats.skipped_filtered += 1;
            continue;
        }

        if file_type.is_dir() {
            backip_stats.entries_created += 1;
            backup_entries.push(BackupEntry {
                relative_path: relative_file_path,
                metadata,
                kind: BackupEntryKind::Directory,
            });
            walk_directory(
                absolute_root,
                &absolute_file_path,
                compiled_filter,
                backip_stats,
                backup_entries,
            )?;
        } else if file_type.is_file() {
            backip_stats.entries_created += 1;
            backup_entries.push(BackupEntry {
                relative_path: relative_file_path,
                metadata,
                kind: BackupEntryKind::File,
            });
        } else if file_type.is_symlink() {
            backip_stats.entries_created += 1;
            backup_entries.push(BackupEntry {
                relative_path: relative_file_path,
                metadata,
                kind: BackupEntryKind::Symlink,
            });
        }
    }
    Ok(())
}

/// Returns paths relative to the source directory.
fn path_traversal(
    absolute_source_path: &Path,
    compiled_filter: &CompiledFilter,
) -> Result<(BackupStats, Vec<BackupEntry>), JobError> {
    let mut backup_stats = BackupStats {
        entries_created: 0,
        skipped_unsupported: 0,
        skipped_filtered: 0,
    };
    let mut backup_entries = Vec::<BackupEntry>::new();
    walk_directory(
        absolute_source_path,
        absolute_source_path,
        compiled_filter,
        &mut backup_stats,
        &mut backup_entries,
    )?;
    Ok((backup_stats, backup_entries))
}

fn backup_one_file_with_content(
    absolute_source_file_path: &Path,
    absolute_backup_file_path: &Path,
    relative_path: &Path,
    content: &Content<'_>,
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
    let mut backup_file_encoder = content
        .writer(backup_file, relative_path)
        .map_err(|error| {
            JobError::internal(format!(
                "failed to create content encoder on backup file {}: {error}",
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
                    "failed to encode and forward data to backup file {}: {error}",
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

fn backup_all_files(
    backup_entries: Vec<BackupEntry>,
    absolute_source_path: &Path,
    absolute_backup_root_directory_path: &Path,
    content: &Content<'_>,
) -> Result<Vec<ManifestEntry>, JobError> {
    let mut manifest_entries = Vec::<ManifestEntry>::new();
    for backup_entry in backup_entries {
        let BackupEntry {
            relative_path,
            metadata,
            kind,
        } = backup_entry;
        let manifest_entry = match kind {
            // The parent directory is always created before writing files.
            BackupEntryKind::File => {
                let absolute_source_file_path = absolute_source_path.join(&relative_path);
                let absolute_backup_file_path =
                    absolute_backup_root_directory_path.join(&relative_path);
                let backup_result = backup_one_file_with_content(
                    &absolute_source_file_path,
                    &absolute_backup_file_path,
                    &relative_path,
                    content,
                )?;
                ManifestEntry {
                    relative_path,
                    metadata,
                    kind: ManifestEntryKind::File {
                        original_size: backup_result.original_size,
                        checksum: backup_result.checksum,
                    },
                }
            }
            BackupEntryKind::Directory => {
                let absolute_backup_directory_path =
                    absolute_backup_root_directory_path.join(&relative_path);
                std::fs::create_dir(&absolute_backup_directory_path).map_err(|error| {
                    JobError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!(
                            "failed to create new backup directory {}: {error}",
                            absolute_backup_directory_path.display(),
                        ),
                    )
                })?;
                ManifestEntry {
                    relative_path,
                    metadata,
                    kind: ManifestEntryKind::Directory,
                }
            }
            BackupEntryKind::Symlink => {
                let absolute_source_file_path = absolute_source_path.join(&relative_path);
                // Do not create a symlink file in backup path
                let target = std::fs::read_link(&absolute_source_file_path).map_err(|error| {
                    JobError::internal(format!(
                        "cannot read symlink target for {}: {error}",
                        absolute_source_file_path.display()
                    ))
                })?;
                ManifestEntry {
                    relative_path,
                    metadata,
                    kind: ManifestEntryKind::Symlink { target },
                }
            }
        };
        manifest_entries.push(manifest_entry);
    }

    for manifest_entry in manifest_entries.iter().rev() {
        if let ManifestEntry {
            relative_path,
            metadata: _,
            kind: ManifestEntryKind::Directory,
        } = manifest_entry
        {
            sync_directory(&absolute_backup_root_directory_path.join(relative_path))?;
        }
    }

    Ok(manifest_entries)
}

pub(super) fn run_with_options(
    source: &Path,
    repository: &Path,
    filter: &Filter,
    backup_id: &str,
    options: &BackupOptions,
    keys: &Keyring,
) -> Result<(), JobError> {
    let content = Content::new(options, keys, backup_id)?;
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
    super::repository::check_existing_format(&absolute_repository_path)?;
    let compiled_filter = filter.compile()?;
    let root_metadata = UnixMetadata::read(&absolute_source_path)?;
    let (backup_stats, backup_entries) = path_traversal(&absolute_source_path, &compiled_filter)?;

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
        backup_entries,
        &absolute_source_path,
        &absolute_backup_root_directory_path,
        &content,
    )?;

    sync_directory(&absolute_backup_root_directory_path)?;
    sync_directory(&absolute_repository_path.join("backups"))?;
    sync_directory(&absolute_repository_path)?;

    let manifest = Manifest {
        format_version: 3,
        backup_id: backup_id.to_owned(),
        created_at,
        source: absolute_source_path,
        compression_algorithm: if options.compression { "zstd" } else { "none" }.to_string(),
        encryption: options.encryption.clone(),
        checksum_algorithm: "sha256".to_string(),
        entries: manifest_entries,
        root_metadata,
        backup_stats,
    };
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|error| JobError::internal(format!("cannot access Tokio runtime: {error}")))?;
    handle.block_on(manifest.write(&absolute_repository_path))?;

    Ok(())
}

#[cfg(test)]
pub(super) fn run(
    source: &Path,
    repository: &Path,
    filter: &Filter,
    backup_id: &str,
) -> Result<(), JobError> {
    run_with_options(
        source,
        repository,
        filter,
        backup_id,
        &BackupOptions::default(),
        &Keyring::default(),
    )
}

#[cfg(test)]
fn backup_one_file(source: &Path, destination: &Path) -> Result<FileBackupResult, JobError> {
    let options = BackupOptions::default();
    let keys = Keyring::default();
    backup_one_file_with_content(
        source,
        destination,
        Path::new("file"),
        &Content::new(&options, &keys, "test")?,
    )
}

#[cfg(test)]
mod tests {
    use super::{super::repository::REPO_SCHEMA, super::storage, *};
    use crate::test_support::{TestDirectory, unfiltered};
    use std::error::Error;

    #[test]
    fn send_file_path_to_run() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let repo = directory.path().join("repo");
        std::fs::write(&source, "hello")?;
        std::fs::create_dir(&repo)?;
        let backup_id = "12345";
        let err = run(&source, &repo, &unfiltered()?, backup_id)
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
        let err = run(&source, &repo, &unfiltered()?, backup_id)
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
        let err = run(&source, &alias, &unfiltered()?, backup_id)
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
        let (stats, entries) = path_traversal(&source, &unfiltered()?.compile()?)?;
        assert_eq!(stats.entries_created, entries.len() as i64);
        assert_eq!(stats.skipped_unsupported, 0);
        assert_eq!(stats.skipped_filtered, 0);
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .any(|entry| entry.relative_path == Path::new("hello.txt")
                    && matches!(entry.kind, BackupEntryKind::File))
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.relative_path == Path::new("empty")
                    && matches!(entry.kind, BackupEntryKind::Directory))
        );
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
        let (stats, entries) = path_traversal(&source, &unfiltered()?.compile()?)?;
        assert_eq!(stats.entries_created, entries.len() as i64);
        assert_eq!(stats.skipped_unsupported, 0);
        assert_eq!(stats.skipped_filtered, 0);
        assert_eq!(entries.len(), 4);
        assert!(
            entries
                .iter()
                .any(|entry| entry.relative_path == Path::new("hello.txt")
                    && matches!(entry.kind, BackupEntryKind::File))
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.relative_path == Path::new("empty")
                    && matches!(entry.kind, BackupEntryKind::Directory))
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.relative_path == Path::new("docs")
                    && matches!(entry.kind, BackupEntryKind::Directory))
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.relative_path == Path::new("docs/README.txt")
                    && matches!(entry.kind, BackupEntryKind::File))
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recursive_traversal_preserves_symlinks() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let source_file = directory.path().join("source/hello.txt");
        let source_docs = directory.path().join("source/docs");
        let alias = directory.path().join("source/docs/alias");
        std::fs::create_dir(&source)?;
        std::fs::write(&source_file, "hello")?;
        std::fs::create_dir(&source_docs)?;
        std::os::unix::fs::symlink(&source_file, &alias)?;
        let (stats, entries) = path_traversal(&source, &unfiltered()?.compile()?)?;
        assert_eq!(stats.entries_created, 3);
        assert_eq!(stats.skipped_unsupported, 0);
        assert_eq!(stats.skipped_filtered, 0);
        assert!(
            entries
                .iter()
                .any(|entry| entry.relative_path == Path::new("docs/alias")
                    && matches!(entry.kind, BackupEntryKind::Symlink))
        );
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
        use std::os::unix::fs::PermissionsExt;
        let timestamp =
            std::time::UNIX_EPOCH + std::time::Duration::new(1_600_000_000, 123_456_789);
        let mut expected_metadata = std::collections::BTreeMap::new();
        for (relative, mode) in [
            ("", 0o750),
            ("hello.txt", 0o751),
            ("dir1", 0o700),
            ("dir1/hello.txt", 0o640),
            ("empty", 0o755),
        ] {
            let path = source.join(relative);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
            std::fs::File::open(&path)?.set_times(
                std::fs::FileTimes::new()
                    .set_accessed(timestamp)
                    .set_modified(timestamp),
            )?;
            expected_metadata.insert(relative, serde_json::to_value(UnixMetadata::read(&path)?)?);
        }

        let source_for_run = source.clone();
        let repository_for_run = repository.clone();

        tokio::task::spawn_blocking(move || {
            run(
                &source_for_run,
                &repository_for_run,
                &unfiltered()?,
                "backup-001",
            )
        })
        .await??;

        let manifest = Manifest::read(&repository, "backup-001").await?;
        assert_eq!(manifest.backup_id, "backup-001");
        assert_eq!(manifest.source, std::fs::canonicalize(&source)?);
        assert_eq!(manifest.format_version, 3);
        assert_eq!(manifest.compression_algorithm, "zstd");
        assert_eq!(manifest.checksum_algorithm, "sha256");
        let created_at = chrono::DateTime::parse_from_rfc3339(&manifest.created_at)?;
        assert_eq!(created_at.offset().local_minus_utc(), 0);
        assert_eq!(manifest.entries.len(), 4);
        assert_eq!(manifest.backup_stats.entries_created, 4);
        assert_eq!(manifest.backup_stats.skipped_unsupported, 0);
        assert_eq!(manifest.backup_stats.skipped_filtered, 0);
        assert_eq!(
            serde_json::to_value(&manifest.root_metadata)?,
            expected_metadata[""]
        );
        for entry in &manifest.entries {
            let relative = entry.relative_path.to_str().ok_or("invalid test path")?;
            assert_eq!(
                serde_json::to_value(&entry.metadata)?,
                expected_metadata[relative]
            );
        }

        let backup_root = repository.join("backups/backup-001");
        for (relative_path, content) in files {
            let (original_size, checksum) = manifest
                .entries
                .iter()
                .find_map(|entry| match entry {
                    ManifestEntry {
                        relative_path: path,
                        kind:
                            ManifestEntryKind::File {
                                original_size,
                                checksum,
                            },
                        ..
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
                ManifestEntry { relative_path: path, kind: ManifestEntryKind::Directory, .. }
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
    async fn symlinks_are_metadata_only_and_unsupported_entries_are_counted()
    -> Result<(), Box<dyn Error>> {
        use std::os::unix::{fs::symlink, net::UnixListener};
        let directory = TestDirectory::new()?;
        let source = directory.path().join("s");
        let repository = directory.path().join("r");
        let outside = directory.path().join("outside");
        std::fs::create_dir(&source)?;
        std::fs::create_dir(&repository)?;
        std::fs::create_dir(&outside)?;
        std::fs::write(outside.join("secret"), b"not part of the backup")?;
        std::fs::write(source.join("payload"), b"contents")?;
        std::fs::create_dir(source.join("links"))?;
        let targets = [
            ("links/relative", PathBuf::from("./../payload")),
            ("links/absolute", outside.clone()),
            ("links/dangling", PathBuf::from("../missing")),
            ("links/cycle", PathBuf::from("cycle")),
            ("links/directory", PathBuf::from("..")),
        ];
        let mut expected_metadata = std::collections::BTreeMap::new();
        for (relative, target) in &targets {
            symlink(target, source.join(relative))?;
            expected_metadata.insert(
                *relative,
                serde_json::to_value(UnixMetadata::read(&source.join(relative))?)?,
            );
        }
        // A real socket exercises skipping without opening an unsupported entry.
        let _listener = UnixListener::bind(source.join("sock"))?;
        let outside_before = serde_json::to_value(UnixMetadata::read(&outside)?)?;
        let source_for_run = source.clone();
        let repository_for_run = repository.clone();
        tokio::task::spawn_blocking(move || {
            run(
                &source_for_run,
                &repository_for_run,
                &unfiltered()?,
                "links",
            )
        })
        .await??;
        let manifest = Manifest::read(&repository, "links").await?;
        assert_eq!(manifest.entries.len(), 7);
        assert_eq!(manifest.backup_stats.entries_created, 7);
        assert_eq!(manifest.backup_stats.skipped_unsupported, 1);
        assert_eq!(manifest.backup_stats.skipped_filtered, 0);
        let backup_root = repository.join("backups/links");
        for (relative, target) in &targets {
            let entry = manifest
                .entries
                .iter()
                .find(|entry| entry.relative_path == Path::new(relative))
                .ok_or_else(|| format!("missing link {relative}"))?;
            match &entry.kind {
                ManifestEntryKind::Symlink { target: stored } => {
                    assert_eq!(stored.as_os_str(), target.as_os_str());
                }
                _ => return Err(format!("incorrect type for {relative}").into()),
            }
            assert_eq!(
                serde_json::to_value(&entry.metadata)?,
                expected_metadata[relative]
            );
            assert_eq!(
                std::fs::symlink_metadata(backup_root.join(relative))
                    .err()
                    .ok_or("created a symlink payload")?
                    .kind(),
                io::ErrorKind::NotFound
            );
            assert_eq!(
                std::fs::read_link(source.join(relative))?.as_os_str(),
                target.as_os_str()
            );
        }
        assert!(!backup_root.join("sock").exists());
        assert_eq!(std::fs::read_dir(backup_root.join("links"))?.count(), 0);
        assert_eq!(
            serde_json::to_value(UnixMetadata::read(&outside)?)?,
            outside_before
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn filters_prune_directories_and_count_only_encountered_entries()
    -> Result<(), Box<dyn Error>> {
        use std::os::unix::{fs::symlink, net::UnixListener};
        let directory = TestDirectory::new()?;
        let source = directory.path().join("s");
        let repository = directory.path().join("r");
        std::fs::create_dir_all(source.join("keep"))?;
        std::fs::create_dir(source.join("blocked"))?;
        std::fs::create_dir(&repository)?;
        std::fs::write(source.join("keep/data.txt"), b"selected")?;
        std::fs::write(source.join("keep/drop.tmp"), b"filtered")?;
        std::fs::write(source.join("blocked/unvisited.txt"), b"not counted")?;
        std::fs::write(source.join("skip.bin"), b"filtered")?;
        symlink("data.txt", source.join("keep/link"))?;
        let _listener = UnixListener::bind(source.join("sock"))?;
        let mut config = crate::test_support::unfiltered_config();
        config["relative_path"] = serde_json::json!({
            "include": ["^keep(/|$)"], "exclude": ["\\.tmp$"]
        });
        let filter: Filter = serde_json::from_value(config)?;
        let source_for_run = source.clone();
        let repository_for_run = repository.clone();
        tokio::task::spawn_blocking(move || {
            run(&source_for_run, &repository_for_run, &filter, "filtered")
        })
        .await??;
        let manifest = Manifest::read(&repository, "filtered").await?;
        let paths: std::collections::BTreeSet<_> = manifest
            .entries
            .iter()
            .map(|entry| entry.relative_path.clone())
            .collect();
        assert_eq!(
            paths,
            ["keep", "keep/data.txt", "keep/link"]
                .into_iter()
                .map(PathBuf::from)
                .collect()
        );
        assert_eq!(manifest.backup_stats.entries_created, 3);
        assert_eq!(manifest.backup_stats.skipped_filtered, 3);
        assert_eq!(manifest.backup_stats.skipped_unsupported, 1);
        let payloads = repository.join("backups/filtered");
        assert!(!payloads.join("blocked").exists());
        assert!(!payloads.join("keep/drop.tmp").exists());
        assert!(std::fs::symlink_metadata(payloads.join("keep/link")).is_err());
        assert_eq!(
            zstd::decode_all(std::fs::File::open(payloads.join("keep/data.txt"))?)?,
            b"selected"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn directory_include_is_required_even_if_descendant_name_matches()
    -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        std::fs::create_dir_all(source.join("docs"))?;
        std::fs::write(source.join("docs/report.txt"), b"unvisited")?;
        let mut config = crate::test_support::unfiltered_config();
        config["relative_path"]["include"] = serde_json::json!(["\\.txt$"]);
        let filter: Filter = serde_json::from_value(config)?;
        let (stats, entries) = path_traversal(&source, &filter.compile()?)?;
        assert!(entries.is_empty());
        assert_eq!(stats.entries_created, 0);
        assert_eq!(stats.skipped_filtered, 1);
        assert_eq!(stats.skipped_unsupported, 0);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn empty_source_publishes_zero_counts() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let repository = directory.path().join("repository");
        std::fs::create_dir(&source)?;
        std::fs::create_dir(&repository)?;
        let source_for_run = source.clone();
        let repository_for_run = repository.clone();
        tokio::task::spawn_blocking(move || {
            run(
                &source_for_run,
                &repository_for_run,
                &unfiltered()?,
                "empty",
            )
        })
        .await??;
        let manifest = Manifest::read(&repository, "empty").await?;
        assert!(manifest.entries.is_empty());
        assert_eq!(manifest.backup_stats.entries_created, 0);
        assert_eq!(manifest.backup_stats.skipped_unsupported, 0);
        assert_eq!(manifest.backup_stats.skipped_filtered, 0);
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
            run(
                &source_for_run,
                &repository_for_run,
                &unfiltered()?,
                "backup-001",
            )
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
            run(
                &source_for_retry,
                &repository_for_retry,
                &unfiltered()?,
                "backup-001",
            )
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
            run(
                &source_for_run,
                &repository_for_run,
                &unfiltered()?,
                "backup-001",
            )
        })
        .await??;
        let expected_manifest =
            serde_json::to_value(Manifest::read(&repository, "backup-001").await?)?;
        let existing_backup_file = repository.join("backups/backup-001/hello.txt");
        let expected_compressed = std::fs::read(&existing_backup_file)?;

        let database_path = repository.join("repository.sqlite3");
        let pool = storage::open(&database_path, REPO_SCHEMA, false).await?;
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
            run(
                &source_for_run,
                &repository_for_run,
                &unfiltered()?,
                "backup-002",
            )
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
        let pool = storage::open(&database_path, REPO_SCHEMA, false).await?;
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
}
