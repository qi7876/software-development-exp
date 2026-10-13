// These integration tests use the real filesystem, compression, and SQLite IO.
// Full operations currently support Unix directory synchronization only.
#![cfg(unix)]

use super::*;
use crate::{backup, test_support::TestDirectory};
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
        tokio::task::spawn_blocking(move || backup::run(&source, &repository, BACKUP_ID)).await??;
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
