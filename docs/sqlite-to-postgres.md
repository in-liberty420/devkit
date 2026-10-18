# SQLite now, Postgres later

Start every project on SQLite. Switch to Postgres when you need more than one
machine (a Fly volume attaches to one machine at a time) or a managed backup and
failover story, not at some traffic number. The switch should be a build
feature, a `DATABASE_URL` change and a data copy, with no application code
change. These rules keep it that way. Paste the short version into each
project's `CLAUDE.md`.

## Short version (for CLAUDE.md)

```markdown
## Database rule (strict)

- SQLite now, Postgres later, zero code change. All database access through
  SeaORM entities and the query builder. No raw SQL strings in application code.
- Migrations via `sea-orm-migration` using only constructs that exist in both:
  enums are text columns validated in Rust, JSON is text, no arrays, no native
  enums, no JSONB operators, no backend-specific functions.
- Validate string lengths in Rust; SQLite ignores `string_len`.
- Don't rely on LIKE being case-insensitive; normalise case in Rust.
- After a constraint error inside a transaction, roll back before the next
  statement.
- Compare stored timestamps only with values read back, or with whole seconds.
- Recognise constraint errors with `devkit_db::is_unique_violation` /
  `is_foreign_key_violation`, never by message text.
- Ids are ULID text (`devkit_db::new_id()`) unless there is a reason for an
  integer key.
- CI runs the database tests on SQLite and on Postgres
  (`TEST_DB_BACKEND=postgres`). New database tests use
  `devkit_db::test_support` so they run on both.
```

## Setup in a new project

```toml
# Cargo.toml of the crate that owns entities and migrations
[features]
postgres = ["devkit-db/postgres"]

[dependencies]
devkit-db = { git = "https://github.com/in-liberty420/devkit", tag = "v0.1.1" } # Cargo.lock pins the commit
sea-orm = { version = "2.0.4", default-features = false, features = ["sqlx-sqlite", "runtime-tokio-rustls", "macros", "with-chrono"] }
sea-orm-migration = { version = "2.0.4", default-features = false, features = ["sqlx-sqlite", "runtime-tokio-rustls"] }

[dev-dependencies]
devkit-db = { git = "https://github.com/in-liberty420/devkit", tag = "v0.1.1", features = ["test-support"] }
```

Cargo.lock records the exact commit the tag pointed at, so a tag moved later
does not change a `--locked` build. For the reusable CI workflow, pin a commit
SHA rather than the tag.

Use the `runtime-tokio-rustls` runtime; SeaORM allows only one runtime per
build. Binaries forward the `postgres` feature, so default builds stay
SQLite-only.

```rust
let db = devkit_db::connect::<Migrator>(&std::env::var("DATABASE_URL")?).await?;
```

Tests:

```rust
let db = devkit_db::test_support::test_db::<Migrator>().await;          // one connection
let shared = devkit_db::test_support::shared_test_db::<Migrator>().await; // real concurrency
```

Run them on Postgres locally:

```bash
docker run -d --rm --name pg -e POSTGRES_USER=dk -e POSTGRES_PASSWORD=dk -p 127.0.0.1:55432:5432 postgres:17-alpine@sha256:b0f9560a2de083e2cc7382e75f808c7381a32852a7ec49117deedb300e552b24
TEST_DB_BACKEND=postgres TEST_DB_POSTGRES_URL=postgres://dk:dk@127.0.0.1:55432/postgres cargo test --features postgres
docker stop pg
```

`TEST_DB_POSTGRES_URL` must point at a loopback host or a local socket, and
each test runs `CREATE DATABASE` there. The check cannot see through a
tunnel: never forward a shared or production server (`ssh -L`,
`kubectl port-forward`, a cloud SQL proxy) to the port you test against.

In CI, call the reusable workflow `.github/workflows/db-tests.yml`; its header
shows how.

## Known differences

Each is pinned in `crates/devkit-db/tests/differences.rs` on both backends.

| Behaviour | SQLite | Postgres | Rule |
|---|---|---|---|
| `string_len(n)` | ignored | enforced | validate lengths in Rust |
| `LIKE` | case-insensitive for ASCII | case-sensitive | normalise case in Rust, or store a lower-cased column |
| Error inside a transaction | later statements still run | transaction aborted until rollback | roll back after any constraint error |
| Timestamp precision | stored as text, keeps what you wrote | microseconds | compare with values read back or whole seconds |
| Unique-violation message | names the columns | names the constraint | use `is_unique_violation`, never the message |
| Column types | values of any type are accepted | strict | let SeaORM types decide; never write a string into an integer column |
| Foreign keys | off unless enabled (`devkit_db::options` turns them on) | always on | always connect through `devkit_db::options` |
| Concurrent writers | one at a time (WAL + 5 s busy timeout) | row-level locks | keep write transactions short |

## Switching a deployed app

1. Build with the `postgres` feature.
2. Create the Postgres database (on Fly: `fly postgres create`, or a managed
   provider) and leave it empty.
3. Stop writes to the app (scale to zero, or a maintenance flag).
4. Run the copy, from a one-off command in the app's own CLI:

   ```rust
   let src = devkit_db::connect::<Migrator>(&sqlite_url).await?;
   let dst = sea_orm::Database::connect(devkit_db::options(&postgres_url)).await?;
   let report = devkit_db::copy::Copy::new()
       .table::<users::Entity>()   // parents first
       .table::<posts::Entity>()
       .run::<Migrator>(&src, &dst)
       .await?;
   ```

   It refuses a source with pending migrations; a destination that is not
   fresh (tables without migration history, or migration history behind this
   build), checked before it migrates anything; a destination table that
   already has rows; and a source table with rows that is not listed. It
   reads the source as one snapshot, writes in one destination transaction,
   continues integer auto-increment sequences after the copied ids, and
   checks every table's row count before committing.
5. Point `DATABASE_URL` at Postgres and start the app. Keep the SQLite file
   until you are satisfied.
