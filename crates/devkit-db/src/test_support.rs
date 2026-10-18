//! A fresh database per test, on the backend the run selects.
//!
//! `TEST_DB_BACKEND` unset or `sqlite` (the default, self-contained) gives
//! SQLite. `TEST_DB_BACKEND=postgres` is the explicit opt-in: it needs the
//! `postgres` feature and `TEST_DB_POSTGRES_URL`, an admin URL on a loopback
//! host (a disposable local server, never a shared one). Each test gets its
//! own database `test_<ulid>`, left for the disposable server to discard.
//! Postgres mode never falls back to SQLite: a misconfiguration panics naming
//! the problem.

use std::path::{Path, PathBuf};

use sea_orm::{Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

pub const BACKEND_VAR: &str = "TEST_DB_BACKEND";
pub const POSTGRES_URL_VAR: &str = "TEST_DB_POSTGRES_URL";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Sqlite,
    Postgres,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Sqlite => "sqlite",
            Backend::Postgres => "postgres",
        }
    }
}

/// The backend named by `TEST_DB_BACKEND`'s value; unset means SQLite.
pub fn parse_backend(value: Option<&str>) -> Result<Backend, String> {
    match value {
        None | Some("sqlite") => Ok(Backend::Sqlite),
        Some("postgres") => Ok(Backend::Postgres),
        Some(other) => Err(format!(
            "{BACKEND_VAR}={other:?}: expected \"sqlite\" or \"postgres\""
        )),
    }
}

/// The backend this test run uses. Panics on an unknown name.
pub fn backend() -> Backend {
    let value = std::env::var(BACKEND_VAR).ok();
    parse_backend(value.as_deref()).unwrap_or_else(|e| panic!("{e}"))
}

/// Fresh database migrated with `M`: in-memory SQLite (one connection) or a
/// new Postgres database.
pub async fn test_db<M: MigratorTrait>() -> DatabaseConnection {
    migrated::<M>(unmigrated_test_db().await).await
}

/// Fresh database with no migrations applied (migration tests start here).
pub async fn unmigrated_test_db() -> DatabaseConnection {
    match backend() {
        Backend::Sqlite => Database::connect(crate::options("sqlite::memory:"))
            .await
            .expect("in-memory test database"),
        Backend::Postgres => postgres_db().await,
    }
}

/// A fresh database migrated with `M` whose pool has several connections, so
/// concurrent calls really race: file-backed SQLite (WAL, busy timeout) in a
/// temporary directory, or a new Postgres database.
pub async fn shared_test_db<M: MigratorTrait>() -> SharedDb {
    match backend() {
        Backend::Sqlite => {
            let mut shared = sqlite_file_db().await;
            shared.db = migrated::<M>(std::mem::take(&mut shared.db)).await;
            shared
        }
        Backend::Postgres => SharedDb {
            db: migrated::<M>(postgres_db().await).await,
            dir: None,
        },
    }
}

/// An empty file-backed SQLite database in its own private temporary
/// directory (created exclusively, mode 0700 on Unix), whatever the selected
/// backend (a copy test's source, for example). Removed on drop.
pub async fn sqlite_file_db() -> SharedDb {
    let dir = std::env::temp_dir().join(format!("devkit-db-test-{}", crate::new_id()));
    private_dir(&dir).expect("temp dir for file-backed SQLite");
    let path = dir.display().to_string();
    // The path goes into a URL: `?` or `#` would start its query or fragment.
    if path.contains(['?', '#']) {
        std::fs::remove_dir_all(&dir).ok();
        panic!("temp dir {path:?} contains '?' or '#'; set TMPDIR to a plain path");
    }
    let url = format!("sqlite://{path}/test.db?mode=rwc");
    // Owns the directory from here on, so a failed connect still removes it.
    let mut shared = SharedDb {
        db: DatabaseConnection::default(),
        dir: Some(dir),
    };
    shared.db = Database::connect(crate::options(&url))
        .await
        .expect("file-backed test database");
    shared
}

#[cfg(unix)]
fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(dir)
}

#[cfg(not(unix))]
fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir(dir)
}

/// A test database; removes its SQLite directory (if any) on drop, after
/// closing the pool.
pub struct SharedDb {
    pub db: DatabaseConnection,
    pub dir: Option<PathBuf>,
}

impl Drop for SharedDb {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.db));
        if let Some(dir) = &self.dir {
            std::fs::remove_dir_all(dir).ok();
        }
    }
}

async fn migrated<M: MigratorTrait>(db: DatabaseConnection) -> DatabaseConnection {
    if let Err(e) = M::up(&db, None).await {
        panic!("migrating the {} test database: {e}", backend().name());
    }
    db
}

#[cfg(not(feature = "postgres"))]
async fn postgres_db() -> DatabaseConnection {
    panic!("{BACKEND_VAR}=postgres needs devkit-db built with `--features postgres`")
}

#[cfg(feature = "postgres")]
async fn postgres_db() -> DatabaseConnection {
    pg::fresh_database().await
}

#[cfg(feature = "postgres")]
mod pg {
    use super::{BACKEND_VAR, POSTGRES_URL_VAR};
    use sea_orm::sqlx::postgres::PgConnectOptions;
    use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection};
    use std::net::IpAddr;
    use std::str::FromStr;
    use std::time::Duration;

    /// Hosts a test run may create databases on: loopback or a local socket.
    pub(super) fn check_local(url: &str) -> Result<(), String> {
        if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
            return Err(format!("{POSTGRES_URL_VAR} must be a postgres:// URL"));
        }
        let opts = PgConnectOptions::from_str(url)
            .map_err(|e| format!("{POSTGRES_URL_VAR} is not a Postgres URL: {e}"))?;
        let host = opts.get_host();
        if is_local_host(host) {
            Ok(())
        } else {
            Err(format!(
                "{POSTGRES_URL_VAR} host {host:?} is not loopback; test databases \
                 are only created on a disposable local server"
            ))
        }
    }

    /// Exactly `localhost`, a Unix socket directory (absolute path), or an IP
    /// literal that is loopback (`127.0.0.0/8`, `::1`). Names are never
    /// prefix-matched: `127.example.com` is a DNS name, not loopback.
    pub(super) fn is_local_host(host: &str) -> bool {
        if host == "localhost" || host.starts_with('/') {
            return true;
        }
        let literal = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host);
        literal.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    }

    fn admin_url() -> String {
        let url = std::env::var(POSTGRES_URL_VAR).unwrap_or_else(|_| {
            panic!("{BACKEND_VAR}=postgres needs {POSTGRES_URL_VAR} (an admin URL on localhost)")
        });
        check_local(&url).unwrap_or_else(|e| panic!("{e}"));
        url
    }

    /// Creates `test_<ulid>` through the admin URL and connects to it.
    pub(super) async fn fresh_database() -> DatabaseConnection {
        let admin_url = admin_url();
        let name = format!("test_{}", crate::new_id().to_lowercase());
        let mut opt = ConnectOptions::new(admin_url.clone());
        opt.max_connections(1)
            .connect_timeout(Duration::from_secs(10))
            .acquire_timeout(Duration::from_secs(10))
            .sqlx_logging(false);
        let admin = Database::connect(opt).await.unwrap_or_else(|e| {
            panic!("{BACKEND_VAR}=postgres: cannot connect to {POSTGRES_URL_VAR}: {e}")
        });
        // Test-only raw SQL: there is no portable way to create a database.
        admin
            .execute_unprepared(&format!("CREATE DATABASE \"{name}\""))
            .await
            .unwrap_or_else(|e| panic!("postgres: CREATE DATABASE {name}: {e}"));
        admin.close().await.ok();
        // Shown with a failing test's output, so the failure names its backend.
        eprintln!("devkit-db test database: postgres {name}");
        let mut opt = crate::options(&admin_url);
        let db_name = name.clone();
        opt.max_connections(4)
            .map_sqlx_postgres_opts(move |o| o.database(&db_name));
        Database::connect(opt)
            .await
            .unwrap_or_else(|e| panic!("postgres: connecting to {name}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_sqlite_selects_sqlite_and_unknown_names_fail() {
        assert_eq!(parse_backend(None), Ok(Backend::Sqlite));
        assert_eq!(parse_backend(Some("sqlite")), Ok(Backend::Sqlite));
        assert_eq!(parse_backend(Some("postgres")), Ok(Backend::Postgres));
        for bad in ["", "Postgres", "pg", "mysql"] {
            let err = parse_backend(Some(bad)).unwrap_err();
            assert!(err.contains(BACKEND_VAR), "{err}");
        }
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn postgres_url_must_be_local() {
        for ok in [
            "postgres://u:p@127.0.0.1:5432/postgres",
            "postgres://u:p@127.1.2.3:5432/postgres",
            "postgres://u:p@localhost/postgres",
            "postgres://u@[::1]:5433/postgres",
            "postgresql://u@[0:0:0:0:0:0:0:1]/postgres",
        ] {
            assert_eq!(pg::check_local(ok), Ok(()), "{ok}");
        }
        for bad in [
            "postgres://u:p@db.example.com/postgres",
            "postgres://u:p@10.0.0.5/postgres",
            "postgres://u:p@127.example.com/postgres",
            "postgres://u:p@127.0.0.1.example.com/postgres",
            "postgres://u:p@localhost.example.com/postgres",
            "postgres://u:p@[::2]/postgres",
            "sqlite::memory:",
        ] {
            assert!(pg::check_local(bad).is_err(), "{bad}");
        }
    }

    #[cfg(feature = "postgres")]
    #[test]
    fn local_host_is_exact_localhost_a_socket_path_or_a_loopback_ip() {
        for ok in [
            "localhost",
            "/var/run/postgresql",
            "/tmp",
            "127.0.0.1",
            "127.255.0.9",
            "::1",
            "[::1]",
        ] {
            assert!(pg::is_local_host(ok), "{ok}");
        }
        for bad in [
            "127.example.com",
            "127.0.0.1.nip.io",
            "localhost.example.com",
            "notlocalhost",
            "128.0.0.1",
            "0.0.0.0",
            "::",
            "::ffff:127.0.0.1",
            "[::1",
            "tmp/socket",
            "",
        ] {
            assert!(!pg::is_local_host(bad), "{bad}");
        }
    }
}
