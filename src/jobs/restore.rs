use super::{
    JobError,
    filesystem::{get_absolute_directory_path, paths_overlap, sync_directory},
    repository::{Manifest, ManifestEntry},
};
use http::StatusCode;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    path::{Component, Path},
};

struct FileBackupResult {
    original_size: u64,
    checksum: String,
}

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

/// restore one file
fn restore_one_file(
    absolute_backup_file_path: &Path,
    absolute_destination_file_path: &Path,
) -> Result<FileBackupResult, JobError> {
    let backup_file = std::fs::OpenOptions::new()
        .read(true)
        .open(absolute_backup_file_path)
        .map_err(|error| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "failed to open backup file {}: {error}",
                    absolute_backup_file_path.display(),
                ),
            )
        })?;
    let mut destination_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(absolute_destination_file_path)
        .map_err(|error| match error.kind() {
            io::ErrorKind::AlreadyExists => JobError::new(
                StatusCode::CONFLICT,
                format!(
                    "destination file {} already exists: {error}",
                    absolute_destination_file_path.display(),
                ),
            ),
            _ => JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "failed to create destination file {}: {error}",
                    absolute_destination_file_path.display(),
                ),
            ),
        })?;

    let mut buffer = [0u8; 128 * 1024];
    let mut backup_file_decoder =
        zstd::stream::read::Decoder::new(backup_file).map_err(|error| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "failed to create zstd decoder on backup file {}: {error}",
                    absolute_backup_file_path.display(),
                ),
            )
        })?;
    let mut hasher = Sha256::new();
    let mut original_size: u64 = 0;

    loop {
        let n = backup_file_decoder.read(&mut buffer).map_err(|error| {
            JobError::internal(format!(
                "failed to decompress and forward data form backup file {} to buffer: {error}",
                absolute_backup_file_path.display(),
            ))
        })?;

        if n == 0 {
            break;
        }

        let chunk = &buffer[..n];
        destination_file.write_all(chunk).map_err(|error| {
            JobError::internal(format!(
                "failed to restore data to destination file {}: {error}",
                absolute_destination_file_path.display(),
            ))
        })?;
        hasher.update(chunk);
        original_size += n as u64;
    }
    destination_file.sync_all().map_err(|error| {
        JobError::internal(format!(
            "failed to sync destination file {}: {error}",
            absolute_destination_file_path.display(),
        ))
    })?;

    Ok(FileBackupResult {
        original_size,
        checksum: hex::encode(hasher.finalize()),
    })
}

/// restore all
fn restore_all_files(
    manifest_entries: &Vec<ManifestEntry>,
    absolute_backup_path: &Path,
    absolute_destination_path: &Path,
) -> Result<(), JobError> {
    for manifest_entry in manifest_entries {
        validate_entry_path(match manifest_entry {
            ManifestEntry::File { path, .. } => path,
            ManifestEntry::Directory { path } => path,
        })?;
    }

    for manifest_entry in manifest_entries {
        match manifest_entry {
            ManifestEntry::File {
                path,
                original_size,
                checksum,
            } => {
                let absolute_backup_file_path = absolute_backup_path.join(path);
                let absolute_destination_file_path = absolute_destination_path.join(path);
                let file_restore_result =
                    restore_one_file(&absolute_backup_file_path, &absolute_destination_file_path)?;
                if &file_restore_result.checksum != checksum
                    || &file_restore_result.original_size != original_size
                {
                    return Err(JobError::internal(format!(
                        "the content of backup file {} is changed after the backup",
                        absolute_backup_file_path.display()
                    )));
                }
            }
            ManifestEntry::Directory { path } => {
                let absolute_destination_directory_path = absolute_destination_path.join(path);
                std::fs::create_dir(&absolute_destination_directory_path).map_err(|error| {
                    JobError::internal(format!(
                        "failed to create new destination directory {}: {error}",
                        absolute_destination_directory_path.display(),
                    ))
                })?;
            }
        }
    }

    for manifest_entry in manifest_entries.iter().rev() {
        if let ManifestEntry::Directory { path } = manifest_entry {
            sync_directory(&absolute_destination_path.join(path))?;
        }
    }

    Ok(())
}

pub fn run(repository: &Path, backup_id: &str, destination: &Path) -> Result<(), JobError> {
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

    let handle = tokio::runtime::Handle::try_current()
        .map_err(|error| JobError::internal(format!("cannot access Tokio runtime: {error}")))?;

    let manifest = handle.block_on(Manifest::read(&absolute_repository_path, backup_id))?;

    if manifest.format_version != 1 {
        return Err(JobError::new(
            StatusCode::BAD_REQUEST,
            "unsupported format verions",
        ));
    }
    if manifest.compression_algorithm != "zstd" {
        return Err(JobError::new(
            StatusCode::BAD_REQUEST,
            "unsupported compression_algorithm",
        ));
    }
    if manifest.checksum_algorithm != "sha256" {
        return Err(JobError::new(
            StatusCode::BAD_REQUEST,
            "unsupported compression_algorithm",
        ));
    }

    let absolute_backup_path = absolute_repository_path.join(format!("backups/{}", backup_id));

    restore_all_files(
        &manifest.entries,
        &absolute_backup_path,
        &absolute_destination_path,
    )?;

    sync_directory(&absolute_destination_path)?;

    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    // These integration tests use the real filesystem, compression, and SQLite IO.
    // Full operations currently support Unix directory synchronization only.
    use super::super::backup;
    use super::*;
    use crate::test_support::TestDirectory;
    use proptest::{
        collection::{btree_map, btree_set, vec},
        prelude::*,
        test_runner::{Config, RngSeed, TestCaseError},
    };
    use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
    use std::{
        collections::{BTreeMap, BTreeSet},
        error::Error,
        future::Future,
        io,
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
    type BackupSnapshot = (Tree, serde_json::Value);

    struct Fixture {
        directory: TestDirectory,
        source: PathBuf,
        repository: PathBuf,
        destination: PathBuf,
    }

    impl Fixture {
        fn new() -> Result<Self, Box<dyn Error>> {
            let directory = TestDirectory::new()?;
            let source = directory.path().join("source");
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
            tokio::task::spawn_blocking(move || backup::run(&source, &repository, BACKUP_ID))
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
            Ok((
                read_tree(&self.repository.join("backups").join(BACKUP_ID))?,
                serde_json::to_value(Manifest::read(&self.repository, BACKUP_ID).await?)?,
            ))
        }

        async fn open_metadata(&self) -> Result<SqliteConnection, sqlx::Error> {
            SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(self.repository.join("repository.sqlite3"))
                    .create_if_missing(false),
            )
            .await
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
                let mut connection = fixture.open_metadata().await?;
                if change_checksum {
                    let mut checksum = hex::encode(Sha256::digest(&contents));
                    let replacement = if checksum.starts_with('0') { "1" } else { "0" };
                    checksum.replace_range(..1, replacement);
                    sqlx::query("UPDATE entries SET checksum = ? WHERE backup_id = ? AND type = 'file'")
                        .bind(checksum).bind(BACKUP_ID).execute(&mut connection).await?;
                } else {
                    sqlx::query("UPDATE entries SET original_size = original_size + ? WHERE backup_id = ? AND type = 'file'")
                        .bind(size_delta).bind(BACKUP_ID).execute(&mut connection).await?;
                }
                connection.close().await?;
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
                let mut connection = fixture.open_metadata().await?;
                sqlx::query("INSERT INTO entries (backup_id, ordinal, type, path, original_size, checksum) VALUES (?, 1, ?, ?, ?, ?)")
                    .bind(BACKUP_ID).bind(kind).bind(serde_json::to_string(&path)?)
                    .bind((kind == "file").then_some(0i64))
                    .bind((kind == "file").then(|| hex::encode(Sha256::digest([]))))
                    .execute(&mut connection).await?;
                connection.close().await?;
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
            let path = fixture
                .repository
                .join("backups")
                .join(BACKUP_ID)
                .join("payload");
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
            assert_eq!(error.code, StatusCode::INTERNAL_SERVER_ERROR);
            assert!(error.message.contains("payload"));
            assert_eq!(fixture.backup_snapshot().await?, backup_before);
            assert_eq!(
                std::fs::read(fixture.source.join("payload"))?,
                b"valid contents"
            );
        }
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
