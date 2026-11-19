mod filter;

use filter::CompiledFilter;
pub(crate) use filter::Filter;

use super::{
    JobError, archive,
    content::{BackupOptions, Content, Keyring},
    filesystem::{device_numbers, get_absolute_directory_path, paths_overlap},
    repository::{
        BackupStats, FORMAT_VERSION, Manifest, ManifestEntry, ManifestEntryKind, PendingArchive,
        UnixMetadata,
    },
};
use chrono::{SecondsFormat, Utc};
use http::StatusCode;
use std::{
    collections::HashMap,
    io,
    os::unix::fs::{FileTypeExt, MetadataExt},
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
    MetadataOnly(ManifestEntryKind),
}

fn walk_directory(
    absolute_root: &Path,
    absolute_current: &Path,
    compiled_filter: &CompiledFilter,
    backup_stats: &mut BackupStats,
    backup_entries: &mut Vec<BackupEntry>,
    original_entries: &mut HashMap<(u64, u64), usize>,
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

        let (stat, metadata) = UnixMetadata::inspect(&absolute_file_path)?;
        let file_type = stat.file_type();
        if file_type.is_socket() {
            backup_stats.skipped_unsupported += 1;
            continue;
        }

        if !compiled_filter.matches(&relative_file_path, &metadata)? {
            backup_stats.skipped_filtered += 1;
            continue;
        }

        let kind = if file_type.is_dir() {
            BackupEntryKind::Directory
        } else if file_type.is_file() {
            BackupEntryKind::File
        } else if file_type.is_symlink() {
            BackupEntryKind::Symlink
        } else if file_type.is_fifo() {
            BackupEntryKind::MetadataOnly(ManifestEntryKind::Fifo)
        } else if file_type.is_char_device() || file_type.is_block_device() {
            let (major, minor) = device_numbers(stat.rdev())?;
            BackupEntryKind::MetadataOnly(if file_type.is_char_device() {
                ManifestEntryKind::CharacterDevice { major, minor }
            } else {
                ManifestEntryKind::BlockDevice { major, minor }
            })
        } else {
            return Err(JobError::internal(format!(
                "unrecognized file type for {}",
                absolute_file_path.display()
            )));
        };
        let (kind, metadata) = if file_type.is_dir() {
            (kind, metadata)
        } else {
            let index = *original_entries
                .entry((stat.dev(), stat.ino()))
                .or_insert(backup_entries.len());
            match backup_entries.get(index) {
                Some(original) => (
                    BackupEntryKind::MetadataOnly(ManifestEntryKind::HardLink {
                        target: original.relative_path.clone(),
                    }),
                    original.metadata.clone(),
                ),
                None => (kind, metadata),
            }
        };
        backup_stats.entries_created += 1;
        backup_entries.push(BackupEntry {
            relative_path: relative_file_path,
            metadata,
            kind,
        });
        if file_type.is_dir() {
            walk_directory(
                absolute_root,
                &absolute_file_path,
                compiled_filter,
                backup_stats,
                backup_entries,
                original_entries,
            )?;
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
        &mut HashMap::new(),
    )?;
    Ok((backup_stats, backup_entries))
}

fn backup_all_files(
    backup_entries: Vec<BackupEntry>,
    absolute_source_path: &Path,
    archive_path: &Path,
    content: &Content<'_>,
) -> Result<(Vec<ManifestEntry>, PendingArchive), JobError> {
    let repository = archive_path
        .parent()
        .ok_or_else(|| JobError::internal("archive has no parent"))?;
    let backup_id = archive_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| JobError::internal("invalid archive filename"))?;
    let mut pending = PendingArchive::create(repository, backup_id)?;
    let pack = || -> io::Result<Vec<ManifestEntry>> {
        let output = pending.output()?;
        let name = archive_path
            .file_name()
            .ok_or_else(|| io::Error::other("archive has no filename"))?;
        let encoder = content.writer(output, Path::new(name))?;
        let mut archive = archive::Writer::new(encoder)?;
        let mut entries = Vec::with_capacity(backup_entries.len());
        for backup_entry in backup_entries {
            let BackupEntry {
                relative_path,
                metadata,
                kind,
            } = backup_entry;
            let source_path = absolute_source_path.join(&relative_path);
            let (kind, mut source) = match kind {
                BackupEntryKind::File => {
                    let file = std::fs::File::open(&source_path).map_err(|error| {
                        io::Error::other(format!("cannot open {}: {error}", source_path.display()))
                    })?;
                    let stat = file.metadata().map_err(|error| {
                        io::Error::other(format!(
                            "cannot read metadata for {}: {error}",
                            source_path.display()
                        ))
                    })?;
                    if !stat.is_file() {
                        return Err(io::Error::other(format!(
                            "source is no longer a regular file: {}",
                            source_path.display()
                        )));
                    }
                    (
                        ManifestEntryKind::File {
                            original_size: stat.len(),
                        },
                        Some(file),
                    )
                }
                BackupEntryKind::Directory => (ManifestEntryKind::Directory, None),
                BackupEntryKind::MetadataOnly(kind) => (kind, None),
                BackupEntryKind::Symlink => (
                    ManifestEntryKind::Symlink {
                        target: std::fs::read_link(&source_path).map_err(|error| {
                            io::Error::other(format!(
                                "cannot read link {}: {error}",
                                source_path.display()
                            ))
                        })?,
                    },
                    None,
                ),
            };
            let entry = ManifestEntry {
                relative_path,
                metadata,
                kind,
            };
            let result = match source.as_mut() {
                Some(file) => archive.append(&entry, file),
                None => archive.append(&entry, &mut io::empty()),
            };
            result.map_err(|error| {
                io::Error::other(format!("cannot pack {}: {error}", source_path.display()))
            })?;
            entries.push(entry);
        }
        archive.finish()?.finish()?;
        Ok(entries)
    };
    pack().map(|entries| (entries, pending)).map_err(|error| {
        JobError::internal(format!(
            "failed to write archive {}: {error}",
            archive_path.display()
        ))
    })
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
    super::filesystem::sync_directory(&absolute_repository_path)?;

    let archive_path = archive::path(&absolute_repository_path, backup_id);

    let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let (manifest_entries, pending) = backup_all_files(
        backup_entries,
        &absolute_source_path,
        &archive_path,
        &content,
    )?;

    let manifest = Manifest {
        format_version: FORMAT_VERSION,
        backup_id: backup_id.to_owned(),
        created_at,
        source: absolute_source_path,
        compression: options.compression.clone(),
        encryption: options.encryption.clone(),
        checksum_algorithm: "sha256".to_string(),
        archive_checksum: String::new(),
        entries: manifest_entries,
        root_metadata,
        backup_stats,
    };
    pending.publish(&manifest)?;

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
mod tests {
    use super::*;
    use crate::test_support::{TestDirectory, unfiltered};
    use sha2::{Digest, Sha256};
    use std::error::Error;

    fn verify_archive(path: &Path, entries: &[ManifestEntry]) -> Result<(), Box<dyn Error>> {
        let mut backup = super::super::repository::ArchiveFile::open(path)?;
        use std::io::{Read, Seek, SeekFrom};
        backup.file.seek(SeekFrom::Start(8))?;
        let decoder = zstd::stream::read::Decoder::new(backup.file.take(backup.payload_len))?;
        let mut archive = archive::Reader::new(decoder)?;
        for entry in entries {
            archive.expect_entry(entry)?;
            if let ManifestEntryKind::File { original_size } = &entry.kind {
                archive.read_file(&mut io::sink(), *original_size)?;
            }
        }
        archive.finish()?;
        Ok(())
    }

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

        let manifest = Manifest::read(&repository, "backup-001")?;
        assert_eq!(manifest.backup_id, "backup-001");
        assert_eq!(manifest.source, std::fs::canonicalize(&source)?);
        assert_eq!(manifest.format_version, FORMAT_VERSION);
        assert!(matches!(
            manifest.compression,
            Some(super::super::content::CompressionOptions::Zstd(_))
        ));
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

        let backup_root = repository.clone();
        for (relative_path, content) in files {
            let original_size = manifest
                .entries
                .iter()
                .find_map(|entry| match entry {
                    ManifestEntry {
                        relative_path: path,
                        kind: ManifestEntryKind::File { original_size },
                        ..
                    } if path.as_path() == Path::new(relative_path) => Some(*original_size),
                    _ => None,
                })
                .ok_or_else(|| format!("missing file entry {relative_path}"))?;
            assert_eq!(original_size, content.len() as u64);
            assert_eq!(std::fs::read(source.join(relative_path))?, content);
        }
        for relative_path in ["dir1", "empty"] {
            assert!(manifest.entries.iter().any(|entry| matches!(
                entry,
                ManifestEntry { relative_path: path, kind: ManifestEntryKind::Directory, .. }
                    if path.as_path() == Path::new(relative_path)
            )));
            assert!(!backup_root.join(relative_path).exists());
            assert!(source.join(relative_path).is_dir());
        }
        assert!(!backup_root.join("backups").exists());
        assert!(archive::path(&repository, "backup-001").is_file());
        let encoded = std::fs::read(archive::path(&repository, "backup-001"))?;
        assert_eq!(
            manifest.archive_checksum,
            hex::encode(Sha256::digest(&encoded[..encoded.len() - 32]))
        );
        assert!(!repository.join("repository.sqlite3").exists());
        verify_archive(&archive::path(&repository, "backup-001"), &manifest.entries)?;
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
        let manifest = Manifest::read(&repository, "links")?;
        assert_eq!(manifest.entries.len(), 7);
        assert_eq!(manifest.backup_stats.entries_created, 7);
        assert_eq!(manifest.backup_stats.skipped_unsupported, 1);
        assert_eq!(manifest.backup_stats.skipped_filtered, 0);
        let backup_root = repository.clone();
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
        assert!(!backup_root.join("backups").exists());
        verify_archive(&archive::path(&repository, "links"), &manifest.entries)?;
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
        let manifest = Manifest::read(&repository, "filtered")?;
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
        let payloads = repository.clone();
        assert!(!payloads.join("blocked").exists());
        assert!(!payloads.join("keep/drop.tmp").exists());
        assert!(std::fs::symlink_metadata(payloads.join("keep/link")).is_err());
        assert!(!payloads.join("backups").exists());
        verify_archive(&archive::path(&repository, "filtered"), &manifest.entries)?;
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn existing_character_device_is_backed_up_as_metadata() -> Result<(), Box<dyn Error>> {
        let repository = TestDirectory::new()?;
        let mut config = crate::test_support::unfiltered_config();
        config["relative_path"]["include"] = serde_json::json!(["^null$"]);
        let filter = serde_json::from_value(config)?;
        run(Path::new("/dev"), repository.path(), &filter, "device")?;
        let manifest = Manifest::read(repository.path(), "device")?;
        let stat = std::fs::symlink_metadata("/dev/null")?;
        let (major, minor) = device_numbers(stat.rdev())?;
        let [entry] = manifest.entries.as_slice() else {
            return Err("expected only the selected character device".into());
        };
        assert_eq!(entry.relative_path, Path::new("null"));
        assert_eq!(
            entry.kind,
            ManifestEntryKind::CharacterDevice { major, minor }
        );
        verify_archive(
            &archive::path(repository.path(), "device"),
            &manifest.entries,
        )?;
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
        let manifest = Manifest::read(&repository, "empty")?;
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
        let expected_manifest = serde_json::to_value(Manifest::read(&repository, "backup-001")?)?;
        let backup_file = repository.join("backup-001.bak");
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
            serde_json::to_value(Manifest::read(&repository, "backup-001")?)?,
            expected_manifest
        );
        assert_eq!(std::fs::read(&backup_file)?, expected_compressed);
        verify_archive(
            &backup_file,
            &Manifest::read(&repository, "backup-001")?.entries,
        )?;
        assert_eq!(std::fs::read(&source_file)?, b"changed");
        Ok(())
    }

    #[cfg(unix)]
    #[cfg(unix)]
    #[tokio::test]
    async fn full_backup_run_does_not_publish_failed_archive() -> Result<(), Box<dyn Error>> {
        use std::os::unix::{ffi::OsStringExt, fs::symlink};
        let directory = TestDirectory::new()?;
        let source = directory.path().join("source");
        let repository = directory.path().join("repository");
        std::fs::create_dir(&source)?;
        std::fs::create_dir(&repository)?;
        std::fs::write(source.join("hello.txt"), b"original")?;
        run(&source, &repository, &unfiltered()?, "backup-001")?;
        let original = std::fs::read(repository.join("backup-001.bak"))?;
        symlink(
            std::ffi::OsString::from_vec(vec![0xff]),
            source.join("invalid-link"),
        )?;
        let error = run(&source, &repository, &unfiltered()?, "backup-002")
            .err()
            .ok_or("accepted invalid path encoding")?;
        assert_eq!(error.code, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!repository.join("backup-002.bak").exists());
        assert_eq!(std::fs::read(repository.join("backup-001.bak"))?, original);
        assert_eq!(
            std::fs::read_dir(&repository)?.count(),
            1,
            "partial output must be cleaned after failure"
        );
        Ok(())
    }
}
