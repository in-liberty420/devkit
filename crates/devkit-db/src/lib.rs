//! SeaORM on SQLite now, Postgres later, with no application code change.
//!
//! - [`connect`] / [`options`]: pool settings that make SQLite safe to use
//!   (WAL, busy timeout, foreign keys, a single connection for in-memory).
//! - [`is_unique_violation`] / [`is_foreign_key_violation`]: constraint errors
//!   recognised the same way on both backends.
//! - [`copy`]: move every row from SQLite to Postgres when it is time to switch.
//! - `test_support` (feature `test-support`): a fresh database per test, on
//!   SQLite by default or Postgres when `TEST_DB_BACKEND=postgres`.
//!
//! The rules that keep the switch a configuration change are in
//! `docs/sqlite-to-postgres.md`.

pub mod connect;
pub mod copy;
#[cfg(feature = "test-support")]
pub mod test_support;

pub use connect::{connect, is_memory_sqlite, migrate, options};

use sea_orm::{DbErr, SqlErr};

/// New ULID string for a row id. Text ids sort by creation time and copy
/// between backends without sequence bookkeeping.
pub fn new_id() -> String {
    ulid::Ulid::new().to_string()
}

/// A unique or primary-key violation, from SeaORM's typed error rather than
/// the message text (which names the constraint on Postgres and the columns
/// on SQLite).
pub fn is_unique_violation(e: &DbErr) -> bool {
    matches!(e.sql_err(), Some(SqlErr::UniqueConstraintViolation(_)))
}

/// A foreign-key violation, recognised the same way as [`is_unique_violation`].
pub fn is_foreign_key_violation(e: &DbErr) -> bool {
    matches!(e.sql_err(), Some(SqlErr::ForeignKeyConstraintViolation(_)))
}
