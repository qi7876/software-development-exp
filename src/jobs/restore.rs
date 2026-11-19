use crate::jobs::repository::ManifestEntryKind;

use super::{
    JobError, archive,
    content::{BackupOptions, Checksum, Content, Keyring},
    filesystem::{encode_device, get_absolute_directory_path, paths_overlap},
    repository::{ArchiveFile, FileTimestamp, ManifestEntry, UnixMetadata},
};
use http::StatusCode;
use nix::{
    fcntl::{AT_FDCWD, AtFlags},
    sys::stat::{Mode, SFlag, mknod},
    unistd::{linkat, mkfifo},
};
use std::{
    fs::{FileTimes, set_times_nofollow},
    io::{self, Read},
    os::unix::fs::{PermissionsExt, lchown},
    path::{Component, Path},
    time::{Duration, UNIX_EPOCH},
};

fn validate_entry_path(path: &Path) -> Result<(), JobError> {
    if path.as_os_str().is_empty()
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(JobError::internal(format!(
            "invalid backup entry path: {}",
            path.display()
        )));
    }

    Ok(())
}

/// Restore one bounded payload from the shared archive stream.
fn restore_one_file(
    archive: &mut archive::Reader<impl Read>,
    destination: &Path,
    original_size: u64,
) -> Result<(), JobError> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| {
            JobError::new(
                if error.kind() == io::ErrorKind::AlreadyExists {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                },
                format!(
                    "failed to create destination file {}: {error}",
                    destination.display()
                ),
            )
        })?;
    archive
        .read_file(&mut file, original_size)
        .map_err(|error| {
            JobError::internal(format!(
                "failed to restore archive payload {}: {error}",
                destination.display()
            ))
        })?;
    // The caller applies metadata and synchronizes this file before success.
    Ok(())
}

fn file_times(path: &Path, metadata: &UnixMetadata) -> Result<FileTimes, JobError> {
    let convert = |timestamp: &FileTimestamp| {
        let seconds = Duration::from_secs(timestamp.seconds.unsigned_abs());
        let nanoseconds = Duration::from_nanos(timestamp.nanoseconds.into());
        let time = if timestamp.seconds < 0 {
            // The manifest stores floor seconds and a positive fractional offset.
            seconds
                .checked_sub(nanoseconds)
                .and_then(|offset| UNIX_EPOCH.checked_sub(offset))
        } else {
            UNIX_EPOCH.checked_add(seconds + nanoseconds)
        };
        time.ok_or_else(|| {
            JobError::internal(format!("timestamp out of range for {}", path.display()))
        })
    };
    Ok(FileTimes::new()
        .set_accessed(convert(&metadata.atime)?)
        .set_modified(convert(&metadata.mtime)?))
}

fn restore_special_node(
    path: &Path,
    metadata: &UnixMetadata,
    creation: io::Result<()>,
) -> Result<(), JobError> {
    creation.map_err(|error| {
        JobError::internal(format!(
            "cannot create special file {}: {error}",
            path.display()
        ))
    })?;
    lchown(path, Some(metadata.uid), Some(metadata.gid)).map_err(|error| {
        JobError::internal(format!(
            "failed to set owner of {}: {error}",
            path.display()
        ))
    })?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(metadata.mode)).map_err(
        |error| {
            JobError::internal(format!(
                "failed to set permissions of {}: {error}",
                path.display()
            ))
        },
    )?;
    set_times_nofollow(path, file_times(path, metadata)?).map_err(|error| {
        JobError::internal(format!(
            "failed to set timestamps of {}: {error}",
            path.display()
        ))
    })
}

/// restore all
fn restore_all_files(
    manifest_entries: &[ManifestEntry],
    backup: &ArchiveFile,
    absolute_destination_path: &Path,
    content: &Content<'_>,
) -> Result<(), JobError> {
    for manifest_entry in manifest_entries {
        validate_entry_path({
            let ManifestEntry {
                relative_path,
                metadata: _,
                kind: _,
            } = manifest_entry;
            relative_path
        })?;
    }

    let archive_name = archive::file_name(&backup.manifest.backup_id);
    let archive_path = Path::new(&archive_name);
    let mut input = Checksum::new(backup.file.try_clone()?);
    let mut magic = [0; 8];
    input.read_exact(&mut magic)?;
    let decoder = content
        .reader((&mut input).take(backup.payload_len), archive_path)
        .map_err(|error| {
            JobError::internal(format!("failed to decode archive payload: {error}"))
        })?;
    let mut archive = archive::Reader::new(decoder).map_err(|error| {
        JobError::internal(format!(
            "failed to read archive payload {}: {error}",
            archive_path.display()
        ))
    })?;

    for manifest_entry in manifest_entries {
        let ManifestEntry {
            relative_path,
            metadata,
            kind,
        } = manifest_entry;
        archive.expect_entry(manifest_entry).map_err(|error| {
            JobError::internal(format!(
                "invalid archive entry {}: {error}",
                relative_path.display()
            ))
        })?;
        let private_mode = Mode::S_IRUSR | Mode::S_IWUSR;
        match kind {
            ManifestEntryKind::File { original_size } => {
                let absolute_destination_file_path = absolute_destination_path.join(relative_path);
                restore_one_file(
                    &mut archive,
                    &absolute_destination_file_path,
                    *original_size,
                )?;

                let file =
                    std::fs::File::open(&absolute_destination_file_path).map_err(|error| {
                        JobError::internal(format!(
                            "failed to open file {}: {error}",
                            absolute_destination_file_path.display()
                        ))
                    })?;

                lchown(
                    &absolute_destination_file_path,
                    Some(metadata.uid),
                    Some(metadata.gid),
                )
                .map_err(|error| {
                    JobError::internal(format!(
                        "failed to set file uid or gid {}: {error}",
                        absolute_destination_file_path.display()
                    ))
                })?;
                file.set_permissions(std::fs::Permissions::from_mode(metadata.mode))
                    .map_err(|error| {
                        JobError::internal(format!(
                            "failed to set file permission {}: {error}",
                            absolute_destination_file_path.display()
                        ))
                    })?;

                file.set_times(file_times(&absolute_destination_file_path, metadata)?)
                    .map_err(|error| {
                        JobError::internal(format!(
                            "failed to set file atime or mtime {}: {error}",
                            absolute_destination_file_path.display()
                        ))
                    })?;
                file.sync_all().map_err(|error| {
                    JobError::internal(format!(
                        "failed to sync file {}: {error}",
                        absolute_destination_file_path.display(),
                    ))
                })?;
            }
            ManifestEntryKind::Directory => {
                let absolute_destination_directory_path =
                    absolute_destination_path.join(relative_path);
                std::fs::create_dir(&absolute_destination_directory_path).map_err(|error| {
                    JobError::internal(format!(
                        "failed to create new destination directory {}: {error}",
                        absolute_destination_directory_path.display(),
                    ))
                })?;
            }
            ManifestEntryKind::Symlink { target } => {
                let absolute_destination_symlink_path =
                    absolute_destination_path.join(relative_path);
                std::os::unix::fs::symlink(target, &absolute_destination_symlink_path).map_err(
                    |error| {
                        JobError::internal(format!(
                            "cannot create symlink {}: {error}",
                            absolute_destination_symlink_path.display()
                        ))
                    },
                )?;
                lchown(
                    &absolute_destination_symlink_path,
                    Some(metadata.uid),
                    Some(metadata.gid),
                )
                .map_err(|error| {
                    JobError::internal(format!(
                        "failed to set symlink uid or gid {}: {error}",
                        absolute_destination_symlink_path.display()
                    ))
                })?;

                set_times_nofollow(
                    &absolute_destination_symlink_path,
                    file_times(&absolute_destination_symlink_path, metadata)?,
                )
                .map_err(|error| {
                    JobError::internal(format!(
                        "failed to set symlink atime or mtime {}: {error}",
                        absolute_destination_symlink_path.display()
                    ))
                })?;
            }
            ManifestEntryKind::Fifo => {
                let path = absolute_destination_path.join(relative_path);
                restore_special_node(
                    &path,
                    metadata,
                    mkfifo(&path, private_mode).map_err(Into::into),
                )?;
            }
            ManifestEntryKind::CharacterDevice { major, minor }
            | ManifestEntryKind::BlockDevice { major, minor } => {
                let path = absolute_destination_path.join(relative_path);
                let device_type = if matches!(kind, ManifestEntryKind::CharacterDevice { .. }) {
                    SFlag::S_IFCHR
                } else {
                    SFlag::S_IFBLK
                };
                let creation = encode_device(*major, *minor).and_then(|device| {
                    mknod(&path, device_type, private_mode, device).map_err(Into::into)
                });
                restore_special_node(&path, metadata, creation)?;
            }
            ManifestEntryKind::HardLink { target } => {
                let original = absolute_destination_path.join(target);
                let alias = absolute_destination_path.join(relative_path);
                linkat(AT_FDCWD, &original, AT_FDCWD, &alias, AtFlags::empty()).map_err(
                    |error| {
                        JobError::internal(format!(
                            "cannot create hard link {} to {}: {error}",
                            alias.display(),
                            original.display(),
                        ))
                    },
                )?;
            }
        }
    }

    archive
        .finish()
        .and_then(|decoder| decoder.finish())
        .map_err(|error| {
            JobError::internal(format!(
                "failed to finish archive payload {}: {error}",
                archive_path.display()
            ))
        })?;

    backup.finish_checksum(input)?;

    for manifest_entry in manifest_entries.iter().rev() {
        if let ManifestEntry {
            relative_path,
            metadata,
            kind: ManifestEntryKind::Directory,
        } = manifest_entry
        {
            let absolute_destination_directory_path =
                &absolute_destination_path.join(relative_path);
            let directory =
                std::fs::File::open(absolute_destination_directory_path).map_err(|error| {
                    JobError::internal(format!(
                        "failed to open directory {}: {error}",
                        absolute_destination_directory_path.display()
                    ))
                })?;
            lchown(
                absolute_destination_directory_path,
                Some(metadata.uid),
                Some(metadata.gid),
            )
            .map_err(|error| {
                JobError::internal(format!(
                    "failed to set directory uid or gid {}: {error}",
                    absolute_destination_directory_path.display()
                ))
            })?;
            directory
                .set_permissions(std::fs::Permissions::from_mode(metadata.mode))
                .map_err(|error| {
                    JobError::internal(format!(
                        "failed to set directory permission {}: {error}",
                        absolute_destination_directory_path.display()
                    ))
                })?;

            directory
                .set_times(file_times(absolute_destination_directory_path, metadata)?)
                .map_err(|error| {
                    JobError::internal(format!(
                        "failed to set directory atime or mtime {}: {error}",
                        absolute_destination_directory_path.display()
                    ))
                })?;
            directory.sync_all().map_err(|error| {
                JobError::internal(format!(
                    "failed to sync directory {}: {error}",
                    absolute_destination_directory_path.display()
                ))
            })?;
        }
    }

    Ok(())
}

pub(super) fn run_with_keys(
    repository: &Path,
    backup_id: &str,
    destination: &Path,
    keys: &Keyring,
) -> Result<(), JobError> {
    let absolute_repository_path = get_absolute_directory_path(repository)?;
    let absolute_destination_path = get_absolute_directory_path(destination)?;
    if paths_overlap(&absolute_repository_path, &absolute_destination_path) {
        return Err(JobError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "repository path {} and destination path {} overlap",
                absolute_repository_path.display(),
                absolute_destination_path.display()
            ),
        ));
    }

    #[cfg(not(unix))]
    sync_directory(&absolute_repository_path)?;

    std::fs::create_dir_all(&absolute_destination_path).map_err(|error| {
        JobError::internal(format!(
            "failed to create destination directory {}: {error}",
            absolute_destination_path.display(),
        ))
    })?;

    // determine whether the destination path is empty or not
    if std::fs::read_dir(&absolute_destination_path)
        .map_err(|error| {
            JobError::internal(format!(
                "failed to read destination path {}: {error}",
                absolute_destination_path.display()
            ))
        })?
        .next()
        .transpose()
        .map_err(|error| {
            JobError::internal(format!(
                "failed to retrieve content from the destination path {}: {error}",
                absolute_destination_path.display()
            ))
        })?
        .is_some()
    {
        return Err(JobError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "destination path {} is not empty",
                absolute_destination_path.display()
            ),
        ));
    }

    let backup = ArchiveFile::read(&absolute_repository_path, backup_id)?;
    let manifest = &backup.manifest;
    let options = BackupOptions {
        compression: manifest.compression.clone(),
        encryption: manifest.encryption.clone(),
    };
    let content = Content::new(&options, keys, &manifest.backup_id)?;
    restore_all_files(
        &manifest.entries,
        &backup,
        &absolute_destination_path,
        &content,
    )?;

    let directory = std::fs::File::open(&absolute_destination_path).map_err(|error| {
        JobError::internal(format!(
            "failed to open directory {}: {error}",
            absolute_destination_path.display()
        ))
    })?;
    lchown(
        &absolute_destination_path,
        Some(manifest.root_metadata.uid),
        Some(manifest.root_metadata.gid),
    )
    .map_err(|error| {
        JobError::internal(format!(
            "failed to set directory uid or gid {}: {error}",
            absolute_destination_path.display()
        ))
    })?;
    directory
        .set_permissions(std::fs::Permissions::from_mode(manifest.root_metadata.mode))
        .map_err(|error| {
            JobError::internal(format!(
                "failed to set directory permission {}: {error}",
                absolute_destination_path.display()
            ))
        })?;

    directory
        .set_times(file_times(
            &absolute_destination_path,
            &manifest.root_metadata,
        )?)
        .map_err(|error| {
            JobError::internal(format!(
                "failed to set directory atime or mtime {}: {error}",
                absolute_destination_path.display()
            ))
        })?;

    directory.sync_all().map_err(|error| {
        JobError::internal(format!(
            "failed to sync directory {}: {error}",
            absolute_destination_path.display()
        ))
    })?;

    Ok(())
}

#[cfg(test)]
pub(super) fn run(repository: &Path, backup_id: &str, destination: &Path) -> Result<(), JobError> {
    run_with_keys(repository, backup_id, destination, &Keyring::default())
}

#[cfg(all(test, unix))]
mod tests {
    // These integration tests use the real filesystem, compression, and SQLite IO.
    // Full operations currently support Unix directory synchronization only.
    use super::super::backup;
    use super::super::repository::Manifest;
    use super::*;
    use crate::test_support::TestDirectory;
    use proptest::{
        collection::{btree_map, btree_set, vec},
        prelude::*,
        test_runner::{Config, RngSeed, TestCaseError},
    };
    use std::{
        collections::{BTreeMap, BTreeSet},
        error::Error,
        future::Future,
        io,
        os::unix::{
            fs::{FileTypeExt, MetadataExt, symlink},
            net::UnixListener,
        },
        path::PathBuf,
    };

    const BACKUP_ID: &str = "backup-001";
    type TestResult = Result<(), Box<dyn Error>>;

    #[derive(Debug, PartialEq, Eq)]
    enum TreeEntry {
        Directory,
        File(Vec<u8>),
    }

    type Tree = BTreeMap<PathBuf, TreeEntry>;
    type BackupSnapshot = Option<Vec<u8>>;

    struct Fixture {
        directory: TestDirectory,
        source: PathBuf,
        repository: PathBuf,
        destination: PathBuf,
    }

    impl Fixture {
        fn new() -> Result<Self, Box<dyn Error>> {
            let directory = TestDirectory::new()?;
            let source = directory.path().join("s");
            let repository = directory.path().join("repository");
            let destination = directory.path().join("restored");
            for path in [&source, &repository, &destination] {
                std::fs::create_dir(path)?;
            }
            Ok(Self {
                directory,
                source,
                repository,
                destination,
            })
        }

        async fn backup(&self) -> TestResult {
            let source = self.source.clone();
            let repository = self.repository.clone();
            tokio::task::spawn_blocking(move || {
                backup::run(
                    &source,
                    &repository,
                    &crate::test_support::unfiltered()?,
                    BACKUP_ID,
                )
            })
            .await??;
            Ok(())
        }

        async fn restore(&self, backup_id: &str) -> Result<(), JobError> {
            let repository = self.repository.clone();
            let destination = self.destination.clone();
            let backup_id = backup_id.to_owned();
            tokio::task::spawn_blocking(move || run(&repository, &backup_id, &destination))
                .await
                .map_err(|error| JobError::internal(format!("restore task failed: {error}")))?
        }

        async fn backup_snapshot(&self) -> Result<BackupSnapshot, Box<dyn Error>> {
            let archive = match std::fs::read(archive::path(&self.repository, BACKUP_ID)) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            Ok(archive)
        }
    }

    fn read_tree(root: &Path) -> io::Result<Tree> {
        fn walk(root: &Path, current: &Path, tree: &mut Tree) -> io::Result<()> {
            for entry in std::fs::read_dir(current)? {
                let entry = entry?;
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .map_err(io::Error::other)?
                    .to_owned();
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    tree.insert(relative, TreeEntry::Directory);
                    walk(root, &path, tree)?;
                } else if kind.is_file() {
                    tree.insert(relative, TreeEntry::File(std::fs::read(&path)?));
                } else {
                    return Err(io::Error::other(format!(
                        "unexpected entry: {}",
                        path.display()
                    )));
                }
            }
            Ok(())
        }
        let mut tree = Tree::new();
        walk(root, root, &mut tree)?;
        Ok(tree)
    }

    fn write_tree(
        root: &Path,
        files: &BTreeMap<PathBuf, Vec<u8>>,
        directories: &BTreeSet<PathBuf>,
    ) -> io::Result<()> {
        for directory in directories {
            std::fs::create_dir_all(root.join(directory))?;
        }
        for (relative, contents) in files {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, contents)?;
        }
        Ok(())
    }

    fn directory_path() -> impl Strategy<Value = PathBuf> {
        vec("[a-z]{1,6}", 1..4).prop_map(|parts| {
            parts
                .into_iter()
                .map(|part| format!("dir_{part}"))
                .collect()
        })
    }

    fn file_path() -> impl Strategy<Value = PathBuf> {
        (vec("[a-z]{1,6}", 0..4), "[a-z]{1,6}").prop_map(|(directories, name)| {
            let mut path: PathBuf = directories
                .into_iter()
                .map(|part| format!("dir_{part}"))
                .collect();
            path.push(format!("file_{name}.bin"));
            path
        })
    }

    fn run_property(future: impl Future<Output = TestResult>) -> Result<(), TestCaseError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        runtime
            .block_on(future)
            .map_err(|error| TestCaseError::fail(error.to_string()))
    }

    async fn check_round_trip(
        files: BTreeMap<PathBuf, Vec<u8>>,
        directories: BTreeSet<PathBuf>,
    ) -> TestResult {
        let fixture = Fixture::new()?;
        write_tree(&fixture.source, &files, &directories)?;
        let original = read_tree(&fixture.source)?;
        fixture.backup().await?;
        let backup_before = fixture.backup_snapshot().await?;
        fixture.restore(BACKUP_ID).await?;
        assert_eq!(read_tree(&fixture.destination)?, original);
        assert_eq!(read_tree(&fixture.source)?, original);
        assert_eq!(fixture.backup_snapshot().await?, backup_before);
        Ok(())
    }

    proptest! {
        // Fixed seeds keep CI reproducible; shrinking retains a minimal failing case.
        #![proptest_config(Config {
            cases: 32,
            rng_seed: RngSeed::Fixed(0xBAC0_2026),
            ..Config::default()
        })]

        #[test]
        fn generated_trees_round_trip(
            files in btree_map(file_path(), vec(any::<u8>(), 0..8193), 0..7),
            directories in btree_set(directory_path(), 0..5),
        ) {
            run_property(check_round_trip(files, directories))?;
        }

        #[test]
        fn occupied_destinations_are_preserved(
            files in btree_map(file_path(), vec(any::<u8>(), 0..4097), 0..5),
            directories in btree_set(directory_path(), 1..4),
        ) {
            run_property(async {
                let fixture = Fixture::new()?;
                std::fs::write(fixture.source.join("payload"), b"backup contents")?;
                fixture.backup().await?;
                let backup_before = fixture.backup_snapshot().await?;
                let source_before = read_tree(&fixture.source)?;
                write_tree(&fixture.destination, &files, &directories)?;
                let occupied = read_tree(&fixture.destination)?;
                let error = fixture.restore(BACKUP_ID).await.err().ok_or("accepted occupied destination")?;
                assert_eq!(error.code, StatusCode::BAD_REQUEST);
                assert_eq!(read_tree(&fixture.destination)?, occupied);
                assert_eq!(read_tree(&fixture.source)?, source_before);
                assert_eq!(fixture.backup_snapshot().await?, backup_before);
                Ok(())
            })?;
        }

        #[test]
        fn metadata_integrity_mismatches_are_rejected(
            contents in vec(any::<u8>(), 0..8193),
            change_checksum in any::<bool>(),
            size_delta in 1i64..100,
        ) {
            run_property(async {
                let fixture = Fixture::new()?;
                std::fs::write(fixture.source.join("payload"), &contents)?;
                fixture.backup().await?;
                let source_before = read_tree(&fixture.source)?;
                let path = archive::path(&fixture.repository, BACKUP_ID);
                if change_checksum {
                    let mut bytes = std::fs::read(&path)?;
                    let index = bytes.len() - 1;
                    bytes[index] ^= 1;
                    std::fs::write(&path, bytes)?;
                } else {
                    crate::test_support::rewrite_backup_metadata(&path, |metadata| {
                        metadata["entries"][0]["kind"]["original_size"] = serde_json::json!(contents.len() as u64 + size_delta as u64);
                    })?;
                }
                let backup_before = fixture.backup_snapshot().await?;
                let error = fixture.restore(BACKUP_ID).await.err().ok_or("accepted mismatched integrity metadata")?;
                assert_eq!(error.code, StatusCode::INTERNAL_SERVER_ERROR);
                assert!(error.message.contains("payload"));
                assert_eq!(read_tree(&fixture.source)?, source_before);
                assert_eq!(fixture.backup_snapshot().await?, backup_before);
                Ok(())
            })?;
        }
    }

    #[test]
    fn copied_backup_restores_without_source_repository_or_async_runtime() -> TestResult {
        let fixture = Fixture::new()?;
        std::fs::write(fixture.source.join("file"), b"standalone contents")?;
        std::fs::create_dir(fixture.source.join("empty"))?;
        let expected = read_tree(&fixture.source)?;
        backup::run(
            &fixture.source,
            &fixture.repository,
            &crate::test_support::unfiltered()?,
            BACKUP_ID,
        )?;
        let portable = fixture.directory.path().join("portable");
        std::fs::create_dir(&portable)?;
        std::fs::copy(
            archive::path(&fixture.repository, BACKUP_ID),
            archive::path(&portable, BACKUP_ID),
        )?;
        std::fs::remove_dir_all(&fixture.source)?;
        std::fs::remove_dir_all(&fixture.repository)?;
        assert_eq!(super::super::repository::list(&portable)?.len(), 1);
        run(&portable, BACKUP_ID, &fixture.destination)?;
        assert_eq!(read_tree(&fixture.destination)?, expected);
        assert_eq!(std::fs::read_dir(portable)?.count(), 1);
        Ok(())
    }

    #[test]
    fn fifo_and_cross_directory_hard_links_round_trip() -> TestResult {
        let fixture = Fixture::new()?;
        let source = &fixture.source;
        std::fs::create_dir(source.join("nested"))?;
        std::fs::write(source.join("file"), b"shared payload")?;
        std::fs::write(source.join("independent"), b"shared payload")?;
        symlink("missing", source.join("link"))?;
        mkfifo(&source.join("pipe"), Mode::S_IRUSR | Mode::S_IWUSR)?;
        std::fs::set_permissions(source.join("pipe"), std::fs::Permissions::from_mode(0o640))?;
        let timestamp = UNIX_EPOCH + Duration::new(1_600_000_000, 123_456_789);
        let times = FileTimes::new()
            .set_accessed(timestamp)
            .set_modified(timestamp);
        set_times_nofollow(source.join("pipe"), times)?;
        for name in ["file", "link", "pipe"] {
            linkat(
                AT_FDCWD,
                &source.join(name),
                AT_FDCWD,
                &source.join("nested").join(name),
                AtFlags::empty(),
            )?;
        }
        std::fs::hard_link(source.join("file"), source.join("excluded"))?;
        let _socket = UnixListener::bind(source.join("sock"))?;

        let mut config = crate::test_support::unfiltered_config();
        config["relative_path"]["exclude"] = serde_json::json!(["^excluded$"]);
        backup::run(
            source,
            &fixture.repository,
            &serde_json::from_value(config)?,
            BACKUP_ID,
        )?;
        let archive = ArchiveFile::read(&fixture.repository, BACKUP_ID)?;
        archive.verify(&Keyring::default())?;
        assert_eq!(archive.manifest.backup_stats.skipped_unsupported, 1);
        assert_eq!(archive.manifest.backup_stats.skipped_filtered, 1);
        assert_eq!(archive.manifest.backup_stats.entries_created, 8);
        let (mut files, mut hard_links, mut fifos) = (0, 0, 0);
        for entry in &archive.manifest.entries {
            match entry.kind {
                ManifestEntryKind::File { original_size } => {
                    assert_eq!(original_size, b"shared payload".len() as u64);
                    files += 1;
                }
                ManifestEntryKind::HardLink { .. } => hard_links += 1,
                ManifestEntryKind::Fifo => fifos += 1,
                _ => {}
            }
        }
        assert_eq!((files, hard_links, fifos), (2, 3, 1));

        run(&fixture.repository, BACKUP_ID, &fixture.destination)?;
        let restored = &fixture.destination;
        for name in ["file", "link", "pipe"] {
            let original = std::fs::symlink_metadata(restored.join(name))?;
            let alias = std::fs::symlink_metadata(restored.join("nested").join(name))?;
            assert_eq!((original.dev(), original.ino()), (alias.dev(), alias.ino()));
            assert_eq!(original.nlink(), 2);
        }
        assert!(
            std::fs::symlink_metadata(restored.join("pipe"))?
                .file_type()
                .is_fifo()
        );
        assert_eq!(
            UnixMetadata::read(&restored.join("pipe"))?,
            UnixMetadata::read(&source.join("pipe"))?
        );
        assert_eq!(
            std::fs::read_link(restored.join("nested/link"))?,
            Path::new("missing")
        );
        assert_eq!(std::fs::read(restored.join("file"))?, b"shared payload");
        std::fs::write(restored.join("nested/file"), b"updated")?;
        assert_eq!(std::fs::read(restored.join("file"))?, b"updated");
        assert_eq!(
            std::fs::read(restored.join("independent"))?,
            b"shared payload"
        );
        for name in ["sock", "excluded"] {
            assert_eq!(
                std::fs::symlink_metadata(restored.join(name))
                    .err()
                    .ok_or("unexpected entry")?
                    .kind(),
                io::ErrorKind::NotFound
            );
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires permission to create character and block device nodes"]
    fn device_nodes_and_hard_links_round_trip() -> TestResult {
        let fixture = Fixture::new()?;
        for (name, kind, major, minor) in [
            ("character", SFlag::S_IFCHR, 1, 3),
            ("block", SFlag::S_IFBLK, 7, 0),
        ] {
            let path = fixture.source.join(name);
            mknod(&path, kind, Mode::S_IRUSR, encode_device(major, minor)?)?;
            std::fs::hard_link(path, fixture.source.join(format!("{name}-alias")))?;
        }
        backup::run(
            &fixture.source,
            &fixture.repository,
            &crate::test_support::unfiltered()?,
            BACKUP_ID,
        )?;
        run(&fixture.repository, BACKUP_ID, &fixture.destination)?;
        for name in ["character", "block"] {
            let path = fixture.destination.join(name);
            let source_path = fixture.source.join(name);
            let original = std::fs::symlink_metadata(&source_path)?;
            let restored = std::fs::symlink_metadata(&path)?;
            let alias =
                std::fs::symlink_metadata(fixture.destination.join(format!("{name}-alias")))?;
            assert_eq!(restored.file_type(), original.file_type());
            assert_eq!(restored.rdev(), original.rdev());
            assert_eq!(
                UnixMetadata::read(&path)?,
                UnixMetadata::read(&source_path)?
            );
            assert_eq!((restored.dev(), restored.ino()), (alias.dev(), alias.ino()));
        }
        Ok(())
    }

    #[test]
    fn v9_and_v10_backups_can_share_a_repository() -> TestResult {
        let fixture = Fixture::new()?;
        let filter = crate::test_support::unfiltered()?;
        std::fs::write(fixture.source.join("file"), b"v9 contents")?;
        symlink("file", fixture.source.join("link"))?;
        backup::run(&fixture.source, &fixture.repository, &filter, "v9")?;
        crate::test_support::rewrite_backup_metadata(
            &archive::path(&fixture.repository, "v9"),
            |manifest| {
                manifest["format_version"] = serde_json::json!(9);
            },
        )?;
        mkfifo(&fixture.source.join("pipe"), Mode::S_IRUSR)?;
        backup::run(&fixture.source, &fixture.repository, &filter, "v10")?;
        assert_eq!(
            super::super::repository::list(&fixture.repository)?.len(),
            2
        );
        for (id, version) in [("v9", 9), ("v10", 10)] {
            let archive = ArchiveFile::read(&fixture.repository, id)?;
            assert_eq!(archive.manifest.format_version, version);
            archive.verify(&Keyring::default())?;
        }
        run(&fixture.repository, "v9", &fixture.destination)?;
        assert_eq!(
            std::fs::read(fixture.destination.join("file"))?,
            b"v9 contents"
        );
        assert_eq!(
            std::fs::read_link(fixture.destination.join("link"))?,
            Path::new("file")
        );
        Ok(())
    }

    #[test]
    fn invalid_hard_link_targets_are_rejected_before_writes() -> TestResult {
        let fixture = Fixture::new()?;
        std::fs::create_dir(fixture.source.join("directory"))?;
        std::fs::write(fixture.source.join("file"), b"contents")?;
        backup::run(
            &fixture.source,
            &fixture.repository,
            &crate::test_support::unfiltered()?,
            BACKUP_ID,
        )?;
        let path = archive::path(&fixture.repository, BACKUP_ID);
        let original = std::fs::read(&path)?;
        let outside = fixture.directory.path().join("outside");
        std::fs::write(&outside, b"outside")?;
        for (target, mismatch, version) in [
            (outside.as_path(), false, 10),
            (Path::new("../outside"), false, 10),
            (Path::new("missing"), false, 10),
            (Path::new("later"), false, 10),
            (Path::new("directory"), false, 10),
            (Path::new("alias"), false, 10),
            (Path::new("file"), true, 10),
            (Path::new("file"), false, 9),
        ] {
            std::fs::write(&path, &original)?;
            crate::test_support::rewrite_backup_metadata(&path, |manifest| {
                let Some(entries) = manifest["entries"].as_array_mut() else {
                    return;
                };
                let Some(file) = entries
                    .iter()
                    .find(|entry| entry["relative_path"] == "file")
                    .cloned()
                else {
                    return;
                };
                let mut alias = file.clone();
                alias["relative_path"] = serde_json::json!("alias");
                alias["kind"] = serde_json::json!({"type":"HardLink", "target":"file"});
                entries.push(alias.clone());
                alias["relative_path"] = serde_json::json!("second-alias");
                alias["kind"]["target"] = serde_json::json!(target);
                if mismatch {
                    alias["metadata"]["mode"] = serde_json::json!(0);
                }
                entries.push(alias);
                let mut later = file;
                later["relative_path"] = serde_json::json!("later");
                entries.push(later);
                manifest["backup_stats"]["entries_created"] = serde_json::json!(entries.len());
                manifest["format_version"] = serde_json::json!(version);
            })?;
            let error = run(&fixture.repository, BACKUP_ID, &fixture.destination)
                .err()
                .ok_or("accepted an invalid hard link")?;
            let expected = if version == 9 {
                "format v9"
            } else {
                "invalid hard link target"
            };
            assert!(error.message.contains(expected), "{error}");
            assert!(read_tree(&fixture.destination)?.is_empty());
        }
        assert_eq!(std::fs::read(outside)?, b"outside");
        Ok(())
    }

    #[tokio::test]
    async fn empty_source_round_trip() -> TestResult {
        check_round_trip(BTreeMap::new(), BTreeSet::new()).await
    }

    #[tokio::test]
    async fn buffer_boundaries_and_empty_directories_round_trip() -> TestResult {
        let mut files: BTreeMap<PathBuf, Vec<u8>> = [0, 1, 131_071, 131_072, 131_073, 300_001]
            .into_iter()
            .map(|size| {
                (
                    PathBuf::from(format!("nested/{size}.bin")),
                    (0u8..=255).cycle().take(size).collect(),
                )
            })
            .collect();
        files.insert(
            PathBuf::from("目录/hello world.txt"),
            b"Unicode path".to_vec(),
        );
        files.insert(PathBuf::from(".hidden"), b"hidden file".to_vec());
        check_round_trip(files, [PathBuf::from("empty/nested")].into()).await
    }

    #[tokio::test]
    async fn archive_round_trip_preserves_unix_metadata_and_link_targets() -> TestResult {
        let fixture = Fixture::new()?;
        std::fs::create_dir(fixture.source.join("nested"))?;
        std::fs::write(fixture.source.join("nested/file"), b"contents")?;
        for (name, target) in [
            ("link", "nested/file"),
            ("dangling", "missing"),
            ("cycle", "cycle"),
        ] {
            std::os::unix::fs::symlink(target, fixture.source.join(name))?;
        }
        let accessed = UNIX_EPOCH + Duration::new(1_600_000_000, 123_456_789);
        let modified = UNIX_EPOCH - Duration::new(1, 234_567_890);
        let times = FileTimes::new()
            .set_accessed(accessed)
            .set_modified(modified);
        for (name, mode) in [("", 0o750), ("nested", 0o750), ("nested/file", 0o400)] {
            let path = fixture.source.join(name);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
            std::fs::set_times(path, times)?;
        }
        fixture.backup().await?;
        let manifest = Manifest::read(&fixture.repository, BACKUP_ID)?;
        fixture.restore(BACKUP_ID).await?;
        assert_eq!(
            super::super::repository::UnixMetadata::read(&fixture.destination)?,
            manifest.root_metadata
        );
        for entry in &manifest.entries {
            let path = fixture.destination.join(&entry.relative_path);
            assert_eq!(
                super::super::repository::UnixMetadata::read(&path)?,
                entry.metadata
            );
            if let ManifestEntryKind::Symlink { target } = &entry.kind {
                assert_eq!(&std::fs::read_link(path)?, target);
            }
        }
        assert_eq!(
            std::fs::read(fixture.destination.join("nested/file"))?,
            b"contents"
        );
        Ok(())
    }

    #[tokio::test]
    async fn invalid_metadata_paths_are_rejected_before_writes() -> TestResult {
        // Put each invalid entry after a valid file to detect validation during writing.
        // Absolute and parent paths target only our temporary directory.
        for kind in ["file", "directory"] {
            for relative in ["", ".", "..", "../escaped", "nested/../escaped", "absolute"] {
                let fixture = Fixture::new()?;
                std::fs::write(fixture.source.join("payload"), b"valid contents")?;
                fixture.backup().await?;
                let escaped = fixture.directory.path().join("escaped");
                std::fs::write(&escaped, b"keep existing data")?;
                let path = if relative == "absolute" {
                    escaped.clone()
                } else {
                    PathBuf::from(relative)
                };
                crate::test_support::rewrite_backup_metadata(
                    &archive::path(&fixture.repository, BACKUP_ID),
                    |metadata| {
                        let mut entry = metadata["entries"][0].clone();
                        entry["relative_path"] = serde_json::json!(path);
                        entry["kind"] = if kind == "file" {
                            serde_json::json!({"type":"File","original_size":0})
                        } else {
                            serde_json::json!({"type":"Directory"})
                        };
                        if let Some(entries) = metadata["entries"].as_array_mut() {
                            entries.push(entry);
                        }
                        metadata["backup_stats"]["entries_created"] = serde_json::json!(2);
                    },
                )?;
                let backup_before = fixture.backup_snapshot().await?;
                let error = fixture
                    .restore(BACKUP_ID)
                    .await
                    .err()
                    .ok_or("accepted invalid metadata path")?;
                assert_eq!(error.code, StatusCode::INTERNAL_SERVER_ERROR);
                assert!(error.message.contains("invalid backup entry path"));
                assert!(read_tree(&fixture.destination)?.is_empty());
                assert_eq!(std::fs::read(&escaped)?, b"keep existing data");
                assert_eq!(
                    std::fs::read(fixture.source.join("payload"))?,
                    b"valid contents"
                );
                assert_eq!(fixture.backup_snapshot().await?, backup_before);
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn damaged_or_missing_payloads_are_rejected() -> TestResult {
        for damage in ["invalid", "truncated", "missing"] {
            let fixture = Fixture::new()?;
            std::fs::write(fixture.source.join("payload"), b"valid contents")?;
            fixture.backup().await?;
            let path = archive::path(&fixture.repository, BACKUP_ID);
            match damage {
                "invalid" => std::fs::write(&path, b"invalid Zstd data")?,
                "truncated" => {
                    let mut contents = std::fs::read(&path)?;
                    contents.pop();
                    std::fs::write(&path, contents)?;
                }
                _ => std::fs::remove_file(&path)?,
            }
            let backup_before = fixture.backup_snapshot().await?;
            let error = fixture
                .restore(BACKUP_ID)
                .await
                .err()
                .ok_or("accepted damaged or missing payload")?;
            assert_eq!(
                error.code,
                if damage == "missing" {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            );
            assert_eq!(fixture.backup_snapshot().await?, backup_before);
            assert_eq!(
                std::fs::read(fixture.source.join("payload"))?,
                b"valid contents"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn whole_archive_checksum_rejects_changed_uncompressed_file_data() -> TestResult {
        let fixture = Fixture::new()?;
        let original = b"unique-original-payload";
        std::fs::write(fixture.source.join("payload"), original)?;
        let source = fixture.source.clone();
        let repository = fixture.repository.clone();
        tokio::task::spawn_blocking(move || {
            backup::run_with_options(
                &source,
                &repository,
                &crate::test_support::unfiltered()?,
                BACKUP_ID,
                &BackupOptions {
                    compression: None,
                    encryption: None,
                },
                &Keyring::default(),
            )
        })
        .await??;

        let path = archive::path(&fixture.repository, BACKUP_ID);
        let mut bytes = std::fs::read(&path)?;
        let position = bytes
            .windows(original.len())
            .position(|window| window == original)
            .ok_or("missing raw file data")?;
        bytes[position] ^= 1;
        std::fs::write(&path, bytes)?;
        let error = fixture
            .restore(BACKUP_ID)
            .await
            .err()
            .ok_or("accepted modified file data")?;
        assert!(
            error.message.contains("archive payload checksum mismatch"),
            "{error}"
        );
        assert_eq!(std::fs::read(fixture.source.join("payload"))?, original);
        Ok(())
    }

    #[tokio::test]
    async fn missing_backup_and_overlapping_paths_are_rejected() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.backup().await?;
        let backup_before = fixture.backup_snapshot().await?;
        let error = fixture
            .restore("missing-backup")
            .await
            .err()
            .ok_or("accepted missing backup")?;
        assert_eq!(error.code, StatusCode::NOT_FOUND);
        assert!(read_tree(&fixture.destination)?.is_empty());

        let child = fixture.repository.join("restore-child");
        std::fs::create_dir(&child)?;
        let alias = fixture.directory.path().join("repository-alias");
        std::os::unix::fs::symlink(&fixture.repository, &alias)?;
        let parent = fixture.directory.path().to_owned();
        for (repository, destination) in [
            (&fixture.repository, &fixture.repository),
            (&fixture.repository, &child),
            (&fixture.repository, &parent),
            (&fixture.repository, &alias),
        ] {
            let error = run(repository, BACKUP_ID, destination)
                .err()
                .ok_or("accepted overlapping paths")?;
            assert_eq!(error.code, StatusCode::BAD_REQUEST);
        }
        assert!(read_tree(&child)?.is_empty());
        assert_eq!(fixture.backup_snapshot().await?, backup_before);
        Ok(())
    }
}
