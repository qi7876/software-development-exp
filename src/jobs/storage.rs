use super::JobError;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{path::Path, time::Duration};

#[derive(Clone, Copy)]
pub struct Schema {
    pub version: u32,
    pub sql: &'static str,
}

// Schema installation and its version are committed together. Connections never
// relax durability: acknowledgement means SQLite has completed the commit.
pub async fn open(path: &Path, schema: Schema, create: bool) -> Result<SqlitePool, JobError> {
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
                sqlx::raw_sql(schema.sql).execute(&mut *transaction).await?;
                sqlx::QueryBuilder::<sqlx::Sqlite>::new("PRAGMA user_version = ")
                    .push(schema.version)
                    .build()
                    .execute(&mut *transaction)
                    .await?;
            }
            version if version == i64::from(schema.version) => {}
            _ => {
                return Err(JobError::internal(format!(
                    "unsupported database schema version {version}; expected {} (no migration is available; use a new database)",
                    schema.version
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
    async fn schema_versions_are_selected_per_database() -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let schema = Schema {
            version: 2,
            sql: "CREATE TABLE test (id INTEGER PRIMARY KEY);",
        };
        let path = directory.path().join("repository.sqlite3");
        let pool = open(&path, schema, true).await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                .fetch_one(&pool)
                .await?,
            2
        );
        pool.close().await;
        open(&path, schema, false).await?.close().await;
        let pool = open(&path, schema, false).await?;
        sqlx::query("PRAGMA user_version = 1")
            .execute(&pool)
            .await?;
        pool.close().await;
        for create in [false, true] {
            let error = open(&path, schema, create)
                .await
                .err()
                .ok_or("accepted v1 database")?;
            assert!(error.message.contains("expected 2"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn database_options_and_schema_versions() -> Result<(), Box<dyn std::error::Error>> {
        let directory = TestDirectory::new()?;
        let path = directory.path().join("test.sqlite3");
        let schema = Schema {
            version: 1,
            sql: "CREATE TABLE test (id INTEGER PRIMARY KEY);",
        };
        let pool = open(&path, schema, true).await?;
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
        let error = open(&path, schema, true)
            .await
            .err()
            .ok_or("accepted unsupported schema")?;
        assert!(error.message.contains("99"));
        assert!(error.message.contains("test.sqlite3"));
        Ok(())
    }
}
