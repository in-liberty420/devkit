use std::time::Duration;

use sea_orm::{ConnectOptions, Database, DatabaseConnection, DbErr};
use sea_orm_migration::MigratorTrait;

/// `sqlite::memory:` or any SQLite URL with `mode=memory`.
pub fn is_memory_sqlite(url: &str) -> bool {
    url.starts_with("sqlite::memory:")
        || (url.starts_with("sqlite:") && url.contains("mode=memory"))
}

/// Connect with [`options`] and run `M`'s pending migrations.
pub async fn connect<M: MigratorTrait>(url: &str) -> Result<DatabaseConnection, DbErr> {
    let db = Database::connect(options(url)).await?;
    migrate::<M>(&db).await?;
    Ok(db)
}

/// Run `M`'s pending migrations.
pub async fn migrate<M: MigratorTrait>(db: &DatabaseConnection) -> Result<(), DbErr> {
    M::up(db, None).await
}

/// Pool options for `url`, without connecting.
///
/// - In-memory SQLite: one connection that never idles out or is recycled,
///   because every connection to `sqlite::memory:` is its own empty database.
///   Dropping the only connection erases the data, so no health check either.
/// - File SQLite: WAL (readers don't block the writer), a 5 s busy timeout
///   (concurrent writers wait instead of failing with `SQLITE_BUSY`), and
///   foreign keys on (SQLite leaves them off by default; Postgres always
///   enforces them).
/// - Anything else (Postgres): driver defaults.
pub fn options(url: &str) -> ConnectOptions {
    let mut opt = ConnectOptions::new(url.to_owned());
    opt.sqlx_logging(false);
    if is_memory_sqlite(url) {
        opt.max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .test_before_acquire(false)
            .map_sqlx_sqlite_opts(|o| o.foreign_keys(true));
    } else if url.starts_with("sqlite:") {
        opt.map_sqlx_sqlite_opts(|o| {
            o.journal_mode(sea_orm::sqlx::sqlite::SqliteJournalMode::Wal)
                .busy_timeout(Duration::from_secs(5))
                .foreign_keys(true)
        });
    }
    opt
}

#[cfg(test)]
mod tests {
    use super::is_memory_sqlite;

    #[test]
    fn memory_urls() {
        assert!(is_memory_sqlite("sqlite::memory:"));
        assert!(is_memory_sqlite("sqlite://file:x?mode=memory&cache=shared"));
        assert!(!is_memory_sqlite("sqlite:///data/app.db?mode=rwc"));
        assert!(!is_memory_sqlite("postgres://u@localhost/db?mode=memory"));
    }
}
