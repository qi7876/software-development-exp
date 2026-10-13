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
