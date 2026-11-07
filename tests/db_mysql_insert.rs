//! Proves `I have "<table>" with …` fills `last_insert_*` correctly on MySQL,
//! which has no `RETURNING` — the bug this task fixes. Runs the compiled
//! host as a subprocess, like `tests/db.rs`, against MySQL and MariaDB
//! (ports 3307/3308 by default).
//!
//! Each case is skipped, not failed, when its DSN env var is unset, so
//! `cargo test` stays runnable without the MySQL/MariaDB containers.
//!
//! Fixture tables live only here (not in `tests/common/db.rs`, which is
//! Postgres-only) — the per-engine fixture set is the next task's job. All
//! five test fns `setup()` (drop + recreate) the SAME shared tables, so —
//! exactly like `tests/common/db.rs::setup` and its own `DB_LOCK` — each
//! setup-through-run_feature critical section holds that lock. Reused rather
//! than duplicated: `common::db` is compiled fresh into this file's own test
//! binary/process, so the lock only ever serializes the test fns in THIS
//! file, never across `tests/db.rs`'s separate process.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "`allow-*-in-tests` covers only `#[test]` functions, not the helpers here"
)]

mod common;

use common::combined;
use common::db::DB_LOCK;
use common::{bddkit_bin, command};
use sqlx::AnyPool;
use std::time::{SystemTime, UNIX_EPOCH};

/// One row per case: env var naming the DSN, and the engine's display name
/// (only used in assertion messages, so a failure says which engine broke).
const ENGINES: &[(&str, &str)] = &[
    ("BDDKIT_TEST_MYSQL_DSN", "mysql"),
    ("BDDKIT_TEST_MARIADB_DSN", "mariadb"),
];

/// Recreates the fixture tables used by this file's cases. Unlike
/// `tests/common/db.rs::setup`, there is no schema to drop: on MySQL/MariaDB
/// a schema IS the connection's own database (§ CLAUDE.md), already created
/// by `docker-compose.yml` (`MYSQL_DATABASE`/`MARIADB_DATABASE: apibdd_it`).
async fn setup(dsn: &str) {
    sqlx::any::install_default_drivers();
    let pool = AnyPool::connect(dsn)
        .await
        .expect("no connection to the test DB; see the README");
    for stmt in [
        "DROP TABLE IF EXISTS composite_ai_rev",
        "DROP TABLE IF EXISTS composite_ai",
        "DROP TABLE IF EXISTS defaulted",
        "DROP TABLE IF EXISTS users_uuid",
        "DROP TABLE IF EXISTS companies_ai",
        "CREATE TABLE companies_ai (id INT AUTO_INCREMENT PRIMARY KEY, slug VARCHAR(255) NOT NULL)",
        "CREATE TABLE users_uuid (id CHAR(36) PRIMARY KEY, email VARCHAR(255) NOT NULL)",
        // A server-side default that is NOT auto-increment: has_default is
        // true, is_identity is false — the one source none of Known/
        // AutoIncrement covers.
        "CREATE TABLE defaulted (id INT NOT NULL DEFAULT 7 PRIMARY KEY, tag VARCHAR(50) NOT NULL)",
        // Composite PK reachable on MySQL: `a` auto-increments (MySQL allows
        // AUTO_INCREMENT as the leading column of a multi-column key), `b`
        // must be given explicitly — mixing both PkSource variants in one row.
        "CREATE TABLE composite_ai (a INT AUTO_INCREMENT, b INT NOT NULL, note VARCHAR(50), PRIMARY KEY (a, b))",
        // Same shape, reversed column definition order: `b` (AUTO_INCREMENT)
        // is declared before `a` (given). pk_columns() follows declaration
        // (ordinal) order, not the PRIMARY KEY clause's own order — this is
        // the arrangement where a name and its source could plausibly
        // disagree if they were ever built as two separately-ordered lists.
        "CREATE TABLE composite_ai_rev (b INT AUTO_INCREMENT, a INT NOT NULL, note VARCHAR(50), PRIMARY KEY (b, a))",
    ] {
        sqlx::query(stmt)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("{stmt}: {e}"));
    }
    pool.close().await;
}

/// Writes a temporary config (connection `default` → `dsn`, no search_path —
/// meaningless and refused on MySQL/MariaDB) and runs the given feature source.
fn run_feature(dsn: &str, feature_src: &str) -> std::process::Output {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("bddkit-db-mysql-{nanos}"));
    std::fs::create_dir_all(dir.join("features")).expect("mkdir");
    std::fs::write(dir.join("features/db.feature"), feature_src).expect("write feature");

    let features_path = dir
        .join("features")
        .display()
        .to_string()
        .replace('\\', "/");
    let cfg =
        format!("paths: [{features_path}]\nresources:\n  db:\n    default:\n      dsn: {dsn}\n");
    let cfg_path = dir.join("cfg.yaml");
    std::fs::write(&cfg_path, cfg).expect("write config");

    let bin = bddkit_bin().expect("callers skip first, with require_bddkit!");
    command(
        &bin,
        &["run", "--config", cfg_path.to_str().expect("path is UTF-8")],
    )
    .output()
    .expect("failed to run bddkit")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_increment_pk_is_read_from_the_insert_result() {
    require_bddkit!();
    for (env, engine) in ENGINES {
        let Ok(dsn) = std::env::var(env) else {
            continue;
        };
        let _guard = DB_LOCK.lock().await;
        setup(&dsn).await;
        // Reading the row back by <<last_insert_id_companies_ai>> proves the id
        // is the real one from the INSERT's own result, not a plausible zero:
        // a wrong/zero id would find no row and the assertion would fail.
        let src = "\
Feature: insert
  Scenario: auto increment
    Given I have \"companies_ai\" with \"slug: acme\"
    Then I should have \"companies_ai\" with \"id: <<last_insert_id_companies_ai>>\"
";
        let out = run_feature(&dsn, src);
        assert!(out.status.success(), "{engine}: {}", combined(&out));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_generated_uuid_pk_lands_in_the_variable() {
    require_bddkit!();
    for (env, engine) in ENGINES {
        let Ok(dsn) = std::env::var(env) else {
            continue;
        };
        let _guard = DB_LOCK.lock().await;
        setup(&dsn).await;
        let src = "\
Feature: insert
  Scenario: uuid pk
    Given I have \"users_uuid\" with \"email: a@b.net\"
    Then I should have \"users_uuid\" with \"id: <<last_insert_id_users_uuid>>\"
";
        let out = run_feature(&dsn, src);
        assert!(out.status.success(), "{engine}: {}", combined(&out));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pk_value_given_in_the_step_is_used_as_is() {
    require_bddkit!();
    for (env, engine) in ENGINES {
        let Ok(dsn) = std::env::var(env) else {
            continue;
        };
        let _guard = DB_LOCK.lock().await;
        setup(&dsn).await;
        let src = "\
Feature: insert
  Scenario: given pk
    Given I have \"companies_ai\" with \"id: 900, slug: given\"
    Then I should have \"companies_ai\" with \"id: <<last_insert_id_companies_ai>>\"
";
        let out = run_feature(&dsn, src);
        assert!(out.status.success(), "{engine}: {}", combined(&out));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_default_that_is_not_auto_increment_fails_naming_the_column() {
    require_bddkit!();
    // MariaDB has RETURNING (`MARIADB.returning == true`, unconditionally on
    // any PK), so it reads the default value straight off the INSERT and
    // never touches the no-RETURNING fallback this test targets. Only real
    // MySQL — no RETURNING at all — hits the "no source for this PK" path.
    for (env, engine) in [("BDDKIT_TEST_MYSQL_DSN", "mysql")] {
        let Ok(dsn) = std::env::var(env) else {
            continue;
        };
        let _guard = DB_LOCK.lock().await;
        setup(&dsn).await;
        let src = "\
Feature: insert
  Scenario: server default pk
    Given I have \"defaulted\" with \"tag: x\"
";
        let out = run_feature(&dsn, src);
        assert!(
            !out.status.success(),
            "{engine}: must fail: {}",
            combined(&out)
        );
        let out = combined(&out);
        assert!(
            out.contains("id"),
            "{engine}: error must name the column: {out}"
        );
        assert!(
            out.contains("server-generated"),
            "{engine}: error must name the reason: {out}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn composite_pk_mixes_auto_increment_and_a_given_value() {
    require_bddkit!();
    for (env, engine) in ENGINES {
        let Ok(dsn) = std::env::var(env) else {
            continue;
        };
        let _guard = DB_LOCK.lock().await;
        setup(&dsn).await;
        // `a` auto-increments (AutoIncrement source), `b` is given (Known
        // source). Both land in one "I should have" so a swapped pairing is
        // caught by the row not matching, not by the two columns happening
        // to hold distinct values.
        let src = "\
Feature: insert
  Scenario: composite pk
    Given I have \"composite_ai\" with \"b: 5, note: linked\"
    Then I should have \"composite_ai\" with \"a: <<last_insert_composite_ai_a>>, b: <<last_insert_composite_ai_b>>\"
";
        let out = run_feature(&dsn, src);
        assert!(out.status.success(), "{engine}: {}", combined(&out));

        // Reversed declaration order (`b` AUTO_INCREMENT declared before `a`
        // given) — see the comment on composite_ai_rev in setup().
        let src_rev = "\
Feature: insert
  Scenario: composite pk, reversed column order
    Given I have \"composite_ai_rev\" with \"a: 7, note: linked\"
    Then I should have \"composite_ai_rev\" with \"a: <<last_insert_composite_ai_rev_a>>, b: <<last_insert_composite_ai_rev_b>>\"
";
        let out = run_feature(&dsn, src_rev);
        assert!(out.status.success(), "{engine}: {}", combined(&out));
    }
}
