//! Copy every row from one database to another through the app's own SeaORM
//! entities: the SQLite → Postgres step of switching backends.
//!
//! ```ignore
//! let src = devkit_db::connect::<Migrator>("sqlite:///data/app.db").await?;
//! let dst = Database::connect(devkit_db::options(&pg_url)).await?;
//! let report = Copy::new()
//!     .table::<users::Entity>()   // parents before children (foreign keys)
//!     .table::<posts::Entity>()
//!     .run::<Migrator>(&src, &dst)
//!     .await?;
//! ```
//!
//! What it guarantees:
//! - the source is at this build's newest migration (checked read-only);
//! - the destination is fresh before anything touches it: either it has no
//!   migration history and none of the listed tables, or it is already at
//!   this build's newest migration (so migrating it never changes an
//!   existing database's schema); every listed table must then be empty;
//! - the source is read inside one transaction, a consistent snapshot, so
//!   all tables are copied as of the same moment;
//! - all rows are written in one destination transaction, and the row
//!   counts are checked inside it, so any failure leaves the destination
//!   empty;
//! - integer auto-increment keys on Postgres continue after the copied ids;
//! - every source table with rows that is not listed is an error, never
//!   silently skipped (SQLite sources).
//!
//! Stop writes to the app before copying; rows written after the snapshot
//! starts are not copied.

use std::collections::BTreeSet;
use std::marker::PhantomData;

use async_trait::async_trait;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    DatabaseTransaction, DbErr, EntityTrait, IdenStatic, IntoActiveModel, Iterable, ModelTrait,
    PaginatorTrait, PrimaryKeyToColumn, PrimaryKeyTrait, QueryFilter, QueryOrder, QuerySelect,
    Statement, TransactionTrait, Value,
};
use sea_orm_migration::{MigratorTrait, SchemaManager};

#[derive(Debug, thiserror::Error)]
pub enum CopyError {
    #[error(transparent)]
    Db(#[from] DbErr),
    #[error("source has pending migrations ({0}); migrate it with this build first")]
    SourceNotMigrated(String),
    #[error("destination is not a fresh database: {0}")]
    DestinationNotFresh(String),
    #[error("destination table {0} is not empty; copy only into a fresh database")]
    DestinationNotEmpty(String),
    #[error("table {table}: source has {expected} rows, copied {copied}")]
    CountMismatch {
        table: String,
        expected: u64,
        copied: u64,
    },
    #[error("source tables not listed for copy: {0}")]
    UnlistedTables(String),
    #[error("table {0} is listed twice")]
    Duplicate(String),
    #[error("{0} is an auto-increment key but Postgres has no sequence for it")]
    NoSequence(String),
}

/// Rows copied per table, in copy order.
#[derive(Debug, Default)]
pub struct Report {
    pub tables: Vec<(String, u64)>,
}

/// A list of tables to copy, in foreign-key order. See the module docs.
pub struct Copy {
    tables: Vec<Box<dyn Table>>,
    batch: u64,
}

impl Default for Copy {
    fn default() -> Self {
        Self::new()
    }
}

impl Copy {
    pub fn new() -> Self {
        Copy {
            tables: Vec::new(),
            batch: 500,
        }
    }

    /// Add an entity's table. List parents before the tables that reference them.
    pub fn table<E>(mut self) -> Self
    where
        E: EntityTrait + Sync,
        E::Model: IntoActiveModel<E::ActiveModel> + Sync,
        E::ActiveModel: ActiveModelTrait<Entity = E> + Send,
    {
        self.tables.push(Box::new(EntityTable::<E>(PhantomData)));
        self
    }

    /// Rows per `INSERT` (default 500). Postgres allows 65 535 bind
    /// parameters per statement, so keep `batch × columns` below that.
    pub fn batch_size(mut self, rows: u64) -> Self {
        self.batch = rows.max(1);
        self
    }

    /// Check both databases, copy every listed table, verify the counts.
    pub async fn run<M: MigratorTrait>(
        &self,
        src: &DatabaseConnection,
        dst: &DatabaseConnection,
    ) -> Result<Report, CopyError> {
        let mut listed = BTreeSet::new();
        for table in &self.tables {
            if !listed.insert(table.name()) {
                return Err(CopyError::Duplicate(table.name()));
            }
        }

        let pending = M::get_pending_migrations_read_only(src).await?;
        if !pending.is_empty() {
            let names: Vec<&str> = pending.iter().map(|m| m.name()).collect();
            return Err(CopyError::SourceNotMigrated(names.join(", ")));
        }
        self.check_fresh::<M>(dst).await?;
        M::up(dst, None).await?;

        // One read transaction: every table is read from the same snapshot.
        let src = src.begin().await?;
        check_unlisted::<M>(&src, &listed).await?;

        // Dropping `dst` without commit rolls back every write below.
        let dst = dst.begin().await?;
        for table in &self.tables {
            if table.count(&dst).await? != 0 {
                return Err(CopyError::DestinationNotEmpty(table.name()));
            }
        }
        let postgres = dst.get_database_backend() == DatabaseBackend::Postgres;
        let mut report = Report::default();
        for table in &self.tables {
            let copied = table.copy(&src, &dst, self.batch).await?;
            if postgres {
                table.reset_sequence(&dst).await?;
            }
            let expected = table.count(&src).await?;
            let written = table.count(&dst).await?;
            if expected != copied || written != copied {
                return Err(CopyError::CountMismatch {
                    table: table.name(),
                    expected,
                    copied: written.min(copied),
                });
            }
            report.tables.push((table.name(), copied));
        }
        dst.commit().await?;
        src.rollback().await?;
        Ok(report)
    }

    /// A destination with migration history must already be at this build's
    /// newest migration; one without must have none of the listed tables.
    /// Either way `M::up` then cannot alter an existing schema.
    async fn check_fresh<M: MigratorTrait>(
        &self,
        dst: &DatabaseConnection,
    ) -> Result<(), CopyError> {
        let schema = SchemaManager::new(dst);
        if schema
            .has_table(M::migration_table_name().to_string())
            .await?
        {
            let pending = M::get_pending_migrations_read_only(dst).await?;
            if !pending.is_empty() {
                let names: Vec<&str> = pending.iter().map(|m| m.name()).collect();
                return Err(CopyError::DestinationNotFresh(format!(
                    "it has migration history with pending migrations ({})",
                    names.join(", ")
                )));
            }
            return Ok(());
        }
        for table in &self.tables {
            if schema.has_table(table.name()).await? {
                return Err(CopyError::DestinationNotFresh(format!(
                    "table {} exists without migration history",
                    table.name()
                )));
            }
        }
        Ok(())
    }
}

/// Every SQLite table with rows must be listed, apart from SeaORM's own
/// migration table (the destination has its own) and SQLite's internal
/// `sqlite_*` tables. Only SQLite sources are checked; the catalog is SQLite's.
async fn check_unlisted<M: MigratorTrait>(
    src: &DatabaseTransaction,
    listed: &BTreeSet<String>,
) -> Result<(), CopyError> {
    if src.get_database_backend() != DatabaseBackend::Sqlite {
        return Ok(());
    }
    let migration_table = M::migration_table_name().to_string();
    // substr, not LIKE: `_` is a LIKE wildcard and LIKE ignores case.
    let rows = src
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' AND substr(name, 1, 7) <> 'sqlite_'",
        ))
        .await?;
    let mut missing = Vec::new();
    for row in rows {
        let name: String = row.try_get("", "name")?;
        if name == migration_table || listed.contains(&name) {
            continue;
        }
        let count = src
            .query_one_raw(Statement::from_string(
                DatabaseBackend::Sqlite,
                format!("SELECT COUNT(*) AS n FROM {}", quote(&name)),
            ))
            .await?
            .map(|r| r.try_get::<i64>("", "n"))
            .transpose()?
            .unwrap_or(0);
        if count > 0 {
            missing.push(name);
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(CopyError::UnlistedTables(missing.join(", ")))
    }
}

/// A double-quoted SQL identifier (valid on SQLite and Postgres).
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[async_trait]
trait Table: Send + Sync {
    fn name(&self) -> String;
    async fn count(&self, db: &DatabaseTransaction) -> Result<u64, DbErr>;
    async fn copy(
        &self,
        src: &DatabaseTransaction,
        dst: &DatabaseTransaction,
        batch: u64,
    ) -> Result<u64, DbErr>;
    async fn reset_sequence(&self, dst: &DatabaseTransaction) -> Result<(), CopyError>;
}

struct EntityTable<E>(PhantomData<fn() -> E>);

#[async_trait]
impl<E> Table for EntityTable<E>
where
    E: EntityTrait + Sync,
    E::Model: IntoActiveModel<E::ActiveModel> + Sync,
    E::ActiveModel: ActiveModelTrait<Entity = E> + Send,
{
    fn name(&self) -> String {
        E::default().table_name().to_owned()
    }

    async fn count(&self, db: &DatabaseTransaction) -> Result<u64, DbErr> {
        E::find().count(db).await
    }

    /// Single-column keys page by key (`WHERE pk > last`), which stays
    /// linear on large tables; composite keys page by offset, which is still
    /// consistent because the source is a snapshot.
    async fn copy(
        &self,
        src: &DatabaseTransaction,
        dst: &DatabaseTransaction,
        batch: u64,
    ) -> Result<u64, DbErr> {
        let keys = key_columns::<E>();
        let mut copied = 0;
        if let [key] = keys[..] {
            let mut after: Option<Value> = None;
            loop {
                let mut query = E::find().order_by_asc(key).limit(batch);
                if let Some(last) = after.take() {
                    query = query.filter(key.gt(last));
                }
                let rows = query.all(src).await?;
                let Some(last) = rows.last() else { break };
                after = Some(last.get(key));
                let full = rows.len() as u64 == batch;
                copied += insert::<E>(rows, dst).await?;
                if !full {
                    break;
                }
            }
        } else {
            let mut query = E::find();
            for key in keys {
                query = query.order_by_asc(key);
            }
            let mut pages = query.paginate(src, batch);
            while let Some(rows) = pages.fetch_and_next().await? {
                copied += insert::<E>(rows, dst).await?;
            }
        }
        Ok(copied)
    }

    /// Explicit ids don't advance a Postgres sequence, so the next default
    /// insert would collide with a copied row. Names travel as bound
    /// parameters; only identifiers, quoted, are part of the SQL text.
    async fn reset_sequence(&self, dst: &DatabaseTransaction) -> Result<(), CopyError> {
        let Some(column) = serial_column::<E>() else {
            return Ok(());
        };
        let table = qualified_table::<E>();
        let sequence: Option<String> = dst
            .query_one_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT pg_get_serial_sequence($1, $2) AS seq",
                [table.clone().into(), column.clone().into()],
            ))
            .await?
            .map(|row| row.try_get::<Option<String>>("", "seq"))
            .transpose()?
            .flatten();
        let Some(sequence) = sequence else {
            return Err(CopyError::NoSequence(format!("{table}.{column}")));
        };
        dst.query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            format!(
                "SELECT setval($1::regclass, COALESCE((SELECT MAX({}) FROM {table}), 0) + 1, false)",
                quote(&column)
            ),
            [sequence.into()],
        ))
        .await?;
        Ok(())
    }
}

async fn insert<E>(rows: Vec<E::Model>, dst: &DatabaseTransaction) -> Result<u64, DbErr>
where
    E: EntityTrait,
    E::Model: IntoActiveModel<E::ActiveModel>,
    E::ActiveModel: ActiveModelTrait<Entity = E> + Send,
{
    let n = rows.len() as u64;
    E::insert_many(rows.into_iter().map(IntoActiveModel::into_active_model))
        .exec_without_returning(dst)
        .await?;
    Ok(n)
}

fn key_columns<E: EntityTrait>() -> Vec<E::Column> {
    E::PrimaryKey::iter().map(|k| k.into_column()).collect()
}

/// `"schema"."table"`, or `"table"` in the connection's default schema.
fn qualified_table<E: EntityTrait>() -> String {
    let entity = E::default();
    match entity.schema_name() {
        Some(schema) => format!("{}.{}", quote(schema), quote(entity.table_name())),
        None => quote(entity.table_name()),
    }
}

/// The key column when `E` has a single integer auto-increment primary key.
fn serial_column<E: EntityTrait>() -> Option<String> {
    if !<E::PrimaryKey as PrimaryKeyTrait>::auto_increment() {
        return None;
    }
    match key_columns::<E>()[..] {
        [key] => Some(key.as_str().to_owned()),
        _ => None,
    }
}
