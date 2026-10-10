use std::{path::Path, time::Duration};

use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

use crate::jobs::JobError;

// Schema installation and its version are committed together. Connections never
// relax durability: acknowledgement means SQLite has completed the commit.
pub(crate) async fn open(
    path: &Path,
    schema: &'static str,
    create: bool,
) -> Result<SqlitePool, JobError> {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(create)
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Full)
                .foreign_keys(true)
                .busy_timeout(Duration::from_secs(5)),
        )
        .await
        .map_err(|error| {
            JobError::internal(format!("cannot open database {}: {error}", path.display()))
        })?;
    let result = async {
        let mut transaction = pool.begin().await?;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&mut *transaction)
            .await?;
        match version {
            0 if create => {
                sqlx::raw_sql(schema).execute(&mut *transaction).await?;
                sqlx::query("PRAGMA user_version = 1")
                    .execute(&mut *transaction)
                    .await?;
            }
            1 => {}
            _ => {
                return Err(JobError::internal(format!(
                    "unsupported database schema version {version}"
                )));
            }
        }
        transaction.commit().await?;
        Ok::<_, JobError>(())
    }
    .await;
    if let Err(error) = result {
        pool.close().await;
        return Err(JobError::internal(format!(
            "cannot initialize database {}: {error}",
            path.display()
        )));
    }
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDirectory;

    #[tokio::test]
    async fn database_options_and_schema_versions() -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let path = directory.path().join("test.sqlite3");
        let pool = open(&path, "CREATE TABLE test (id INTEGER PRIMARY KEY);", true).await?;
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&pool)
            .await?;
        assert_eq!(journal, "wal");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA synchronous")
                .fetch_one(&pool)
                .await?,
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
                .fetch_one(&pool)
                .await?,
            1
        );
        sqlx::query("PRAGMA user_version = 99")
            .execute(&pool)
            .await?;
        pool.close().await;
        let error = open(&path, "", true)
            .await
            .err()
            .ok_or("accepted unsupported schema")?;
        assert!(error.message.contains("99"));
        assert!(error.message.contains("test.sqlite3"));
        Ok(())
    }
}
