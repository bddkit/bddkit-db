//! Proves `Db::connect` picks the right `Platform` per connection, against
//! real engines. Runs the host binary as a subprocess (like `tests/db.rs`),
//! so the platform is picked by the plugin as a suite meets it.
//!
//! Each engine's dialect is told apart from stderr in debug mode, using
//! `I get next value of sequence "s" as "x"`:
//! - Postgres generates `SELECT nextval($1::regclass)::text` (its own
//!   `next_sequence`), whether or not the sequence actually exists.
//! - MariaDB generates `SELECT CAST(NEXTVAL(s) AS CHAR)` — the flag that
//!   makes it, and only it, support sequences among the two MySQL statics.
//! - MySQL never gets that far: `Platform::next_sequence` returns `None`, and
//!   `ops::next_sequence` reports "sequences are not supported on mysql" —
//!   the platform's own `name()`, in the error text.
//!
//! Each case is skipped, not failed, when its DSN env var is unset, so
//! `cargo test` stays runnable on a Postgres-only machine.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "`allow-*-in-tests` covers only `#[test]` functions, not the helpers here"
)]

mod common;

use common::{bddkit_bin, combined, command};
use std::time::{SystemTime, UNIX_EPOCH};

fn run_against(dsn: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("bddkit-db-engines-{nanos}"));
    std::fs::create_dir_all(dir.join("features")).expect("mkdir");
    std::fs::write(
        dir.join("features/db.feature"),
        "Feature: platform selection\n  \
         Scenario: probe the dialect\n    \
         Given I am in debug mode\n    \
         Then I get next value of sequence \"s\" as \"x\"\n",
    )
    .expect("write feature");

    let features_path = dir
        .join("features")
        .display()
        .to_string()
        .replace('\\', "/");
    // No search_path: it is meaningless (and refused) on MySQL/MariaDB, and
    // this test never needs schema-qualified names.
    let cfg =
        format!("paths: [{features_path}]\nresources:\n  db:\n    default:\n      dsn: {dsn}\n");
    let cfg_path = dir.join("cfg.yaml");
    std::fs::write(&cfg_path, cfg).expect("write config");

    let bin = bddkit_bin().expect("callers skip first, with require_bddkit!");
    let out = command(
        &bin,
        &["run", "--config", cfg_path.to_str().expect("path is UTF-8")],
    )
    .output()
    .expect("failed to run bddkit");
    combined(&out)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_engine_selects_its_own_platform() {
    require_bddkit!();
    for (env, expected_signal) in [
        ("BDDKIT_TEST_DSN", "SQL: SELECT nextval($1::regclass)::text"),
        (
            "BDDKIT_TEST_MYSQL_DSN",
            "sequences are not supported on mysql",
        ),
        (
            "BDDKIT_TEST_MARIADB_DSN",
            "SQL: SELECT CAST(NEXTVAL(s) AS CHAR)",
        ),
    ] {
        let Ok(dsn) = std::env::var(env) else {
            continue;
        };
        let out = run_against(&dsn);
        assert!(
            out.contains(expected_signal),
            "{env}: expected {expected_signal:?} in output:\n{out}"
        );
    }
}
