//! Helpers for DB integration tests: recreating the fixture schema and running
//! the host against a temporary config (like the M1 acceptance tests).

use sqlx::AnyPool;
use std::process::{Command, Output};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, MutexGuard};

/// The `apibdd_it` schema is shared across all tests; serialize them so that
/// one test recreating the fixture does not collide with another.
pub(crate) static DB_LOCK: Mutex<()> = Mutex::const_new(());

#[derive(Clone, Copy, Default)]
struct TimingRow {
    lock: Duration,
    connect: Duration,
    schema: Duration,
    close: Duration,
    files: Duration,
    bddkit: Duration,
    total: Duration,
}

impl TimingRow {
    fn markdown(self, test: &str) -> String {
        let ms = |d: Duration| d.as_secs_f64() * 1_000.0;
        format!(
            "| test | lock | connect | schema | close | files | bddkit | total |\n| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n| {test} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
            ms(self.lock),
            ms(self.connect),
            ms(self.schema),
            ms(self.close),
            ms(self.files),
            ms(self.bddkit),
            ms(self.total),
        )
    }
}

fn timings_enabled(value: Option<&str>) -> bool {
    value == Some("1")
}

pub struct Setup {
    _guard: MutexGuard<'static, ()>,
    started: Instant,
    timings: TimingRow,
}

/// The engine the DB suite runs against, chosen by `BDDKIT_TEST_ENGINE`.
/// Postgres by default, so a plain `cargo test` keeps the behavior — and the
/// DSN — it had before the suite became engine-aware. CI runs the job once
/// per value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine {
    Postgres,
    MySql,
    MariaDb,
}

pub fn engine() -> Engine {
    match std::env::var("BDDKIT_TEST_ENGINE")
        .unwrap_or_default()
        .as_str()
    {
        "" | "postgres" => Engine::Postgres,
        "mysql" => Engine::MySql,
        "mariadb" => Engine::MariaDb,
        other => {
            panic!("unknown BDDKIT_TEST_ENGINE {other:?} (expected postgres, mysql or mariadb)")
        }
    }
}

/// Each engine keeps the DSN variable it already had elsewhere in the suite
/// (`tests/db_engines.rs`, `tests/db_mysql_insert.rs`), so one exported DSN
/// serves every test binary.
pub fn test_dsn() -> String {
    let (var, default) = match engine() {
        Engine::Postgres => (
            "BDDKIT_TEST_DSN",
            "postgres://postgres:postgres@127.0.0.1:5433/postgres",
        ),
        Engine::MySql => (
            "BDDKIT_TEST_MYSQL_DSN",
            "mysql://root:root@127.0.0.1:3307/apibdd_it",
        ),
        Engine::MariaDb => (
            "BDDKIT_TEST_MARIADB_DSN",
            "mysql://root:root@127.0.0.1:3308/apibdd_it",
        ),
    };
    std::env::var(var).unwrap_or_else(|_| default.to_string())
}

/// The fixture DDL: the same three tables and one sequence everywhere, each
/// spelled the way its engine spells it. `users.id` is the point of the set —
/// `uuid`, `char(36)` and MariaDB's native `UUID` are the three branches of
/// `Platform::wants_client_uuid`, reached from one unchanged feature source.
fn fixture_ddl() -> Vec<&'static str> {
    match engine() {
        Engine::Postgres => vec![
            "DROP SCHEMA IF EXISTS apibdd_it CASCADE",
            "CREATE SCHEMA apibdd_it",
            "CREATE TABLE apibdd_it.companies (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
             slug text NOT NULL, name text, created_at timestamptz NOT NULL DEFAULT now())",
            "CREATE TABLE apibdd_it.users (id uuid PRIMARY KEY, email text NOT NULL, \
             name text, created_at timestamptz NOT NULL, deleted_at timestamptz)",
            "CREATE TABLE apibdd_it.pair (a int NOT NULL, b int NOT NULL, note text, PRIMARY KEY (a, b))",
            "CREATE SEQUENCE apibdd_it.thing_seq",
        ],
        // On MySQL/MariaDB a schema IS the connection's own database, already
        // created by docker-compose, so the tables are dropped one by one
        // instead of the whole schema. MySQL has no sequences at all — the
        // step is expected to fail there, and that is what the suite asserts.
        Engine::MySql => vec![
            "DROP TABLE IF EXISTS companies",
            "DROP TABLE IF EXISTS users",
            "DROP TABLE IF EXISTS pair",
            "CREATE TABLE companies (id int AUTO_INCREMENT PRIMARY KEY, \
             slug varchar(255) NOT NULL, name varchar(255), \
             created_at datetime NOT NULL DEFAULT CURRENT_TIMESTAMP)",
            "CREATE TABLE users (id char(36) PRIMARY KEY, email varchar(255) NOT NULL, \
             name varchar(255), created_at datetime NOT NULL, deleted_at datetime)",
            "CREATE TABLE pair (a int NOT NULL, b int NOT NULL, note varchar(255), PRIMARY KEY (a, b))",
        ],
        Engine::MariaDb => vec![
            "DROP TABLE IF EXISTS companies",
            "DROP TABLE IF EXISTS users",
            "DROP TABLE IF EXISTS pair",
            "DROP SEQUENCE IF EXISTS thing_seq",
            "CREATE TABLE companies (id int AUTO_INCREMENT PRIMARY KEY, \
             slug varchar(255) NOT NULL, name varchar(255), \
             created_at datetime NOT NULL DEFAULT CURRENT_TIMESTAMP)",
            "CREATE TABLE users (id UUID PRIMARY KEY, email varchar(255) NOT NULL, \
             name varchar(255), created_at datetime NOT NULL, deleted_at datetime)",
            "CREATE TABLE pair (a int NOT NULL, b int NOT NULL, note varchar(255), PRIMARY KEY (a, b))",
            "CREATE SEQUENCE thing_seq",
        ],
    }
}

/// Recreates the fixture (tables for every PK case) on the engine under test.
/// Returns a guard: hold it until the end of the test for isolation.
pub async fn setup() -> Setup {
    let started = Instant::now();
    let phase = Instant::now();
    let guard = DB_LOCK.lock().await;
    let lock = phase.elapsed();
    let phase = Instant::now();
    // This test binary is a separate process from `bddkit` and builds its own
    // pool, so it needs its own driver installation (AnyPool panics without it).
    sqlx::any::install_default_drivers();
    let pool = AnyPool::connect(&test_dsn())
        .await
        .expect("no connection to the test DB; see the README");
    let connect = phase.elapsed();
    let phase = Instant::now();
    for stmt in fixture_ddl() {
        sqlx::query(stmt)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("{stmt}: {e}"));
    }
    let schema = phase.elapsed();
    let phase = Instant::now();
    pool.close().await;
    Setup {
        _guard: guard,
        started,
        timings: TimingRow {
            lock,
            connect,
            schema,
            close: phase.elapsed(),
            ..Default::default()
        },
    }
}

/// The `resources.db` entry of one connection to the test database. On
/// MySQL/MariaDB a schema is a database — the DSN already names `apibdd_it`,
/// and a search_path there is refused at startup by design.
pub fn connection_yaml(name: &str) -> String {
    let search_path = match engine() {
        Engine::Postgres => "      search_path: [apibdd_it]\n",
        Engine::MySql | Engine::MariaDb => "",
    };
    format!("    {name}:\n      dsn: {}\n{search_path}", test_dsn())
}

/// Writes a temporary config (connection `default` → the test DB of the
/// engine under test) and returns a configured binary command.
pub fn feature_command(feature_src: &str) -> Command {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("bddkit-db-{nanos}"));
    std::fs::create_dir_all(dir.join("features")).expect("mkdir");
    std::fs::write(dir.join("features/db.feature"), feature_src).expect("write feature");

    let features_path = dir
        .join("features")
        .display()
        .to_string()
        .replace('\\', "/");
    let cfg = format!(
        "paths: [{features_path}]\nresources:\n  db:\n{}",
        connection_yaml("default")
    );
    let cfg_path = dir.join("cfg.yaml");
    std::fs::write(&cfg_path, cfg).expect("write config");

    let bin = super::bddkit_bin().expect("callers skip first, with require_bddkit!");
    super::command(
        &bin,
        &["run", "--config", cfg_path.to_str().expect("path is UTF-8")],
    )
}

/// Runs the feature command and returns the output.
#[track_caller]
pub fn run_feature(feature_src: &str, setup: &Setup) -> Output {
    let phase = Instant::now();
    let mut command = feature_command(feature_src);
    let files = phase.elapsed();
    let phase = Instant::now();
    let out = command.output().expect("failed to run bddkit");
    let timings = TimingRow {
        files,
        bddkit: phase.elapsed(),
        total: setup.started.elapsed(),
        ..setup.timings
    };
    if timings_enabled(std::env::var("BDDKIT_TEST_TIMINGS").ok().as_deref()) {
        let caller = std::panic::Location::caller();
        eprintln!(
            "{}",
            timings.markdown(&format!("{}:{}", caller.file(), caller.line()))
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_row_reports_all_phases() {
        let row = TimingRow::default().markdown("example");
        assert!(
            row.contains("lock")
                && row.contains("connect")
                && row.contains("schema")
                && row.contains("files")
                && row.contains("bddkit")
                && row.contains("total"),
            "{row}"
        );
        assert_eq!(row.lines().count(), 3, "{row}");
    }

    #[test]
    fn timings_are_enabled_only_by_one() {
        assert!(timings_enabled(Some("1")));
        assert!(!timings_enabled(None));
        assert!(!timings_enabled(Some("true")));
    }
}
