//! The real `bddkit` binary, built with `--features dev-unreserve` so that a
//! plugin may claim `db`, and this crate's `cdylib`. `BDDKIT_BIN` names the
//! host. Without it the tests skip, except under `CI`, where a missing host
//! is a failure: a skipped suite must never look green there.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "`allow-*-in-tests` covers only `#[test]` functions, not the helpers here"
)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

use common::bddkit;
use serde_json::Value;

fn show(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

const FEATURE: &str = "Feature: only\n  Scenario: one\n    Given set variable \"x\" to \"1\"\n";

/// A project whose config declares `resources:` as `db_section` and whose one
/// feature needs no resource, so every outcome comes from the connections.
fn project(name: &str, db_section: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bddkit-db-{name}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("features")).expect("mkdir");
    std::fs::write(dir.join("features/only.feature"), FEATURE).expect("feature");
    let config = dir.join("cfg.yaml");
    std::fs::write(
        &config,
        format!(
            "paths: [{}]\nresources:\n  api: {{}}\n{db_section}",
            dir.join("features")
                .display()
                .to_string()
                .replace('\\', "/")
        ),
    )
    .expect("config");
    config
}

/// `default_db` is a top-level key, needed once several connections are declared.
fn default_db(config: PathBuf, name: &str) -> PathBuf {
    let mut text = std::fs::read_to_string(&config).expect("read config");
    text.push_str(&format!("default_db: {name}\n"));
    std::fs::write(&config, text).expect("write config");
    config
}

fn path(p: &Path) -> &str {
    p.to_str().expect("path is UTF-8")
}

fn dsn(variable: &str, default: &str) -> String {
    std::env::var(variable).unwrap_or_else(|_| default.to_string())
}

fn postgres_dsn() -> String {
    dsn(
        "BDDKIT_TEST_DSN",
        "postgres://postgres:postgres@127.0.0.1:5433/postgres",
    )
}

// A DSN password that equals an engine name masks that word in `doctor`: the
// host masks by value, which is why the engine rows below use MySQL and MariaDB.
/// An unreachable DB is rejected BEFORE the first request, so code 2, not 1.
#[test]
fn an_unreachable_database_fails_the_startup_with_code_two() {
    let bin = require_bddkit!();
    let cfg = project(
        "unreachable",
        "  db:\n    main:\n      dsn: postgres://nobody:nobody@127.0.0.1:1/nothing\n",
    );
    let out = bddkit(&bin, &["run", "--config", path(&cfg)]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{}", show(&out));
    assert!(stderr.contains("run not started"), "{stderr}");
    assert!(
        stderr.contains("main"),
        "the connection is named:\n{stderr}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("run "),
        "no run id is printed"
    );
}

#[test]
fn a_dead_connection_after_a_live_one_stops_the_run_and_is_named() {
    // Connections open in name order, so `a_live` is already a pool when `z_dead` fails.
    let bin = require_bddkit!();
    let cfg = default_db(
        project(
            "one-dead",
            &format!(
                "  db:\n    a_live:\n      dsn: {}\n    z_dead:\n      dsn: postgres://u:p@127.0.0.1:1/x\n",
                postgres_dsn()
            ),
        ),
        "a_live",
    );
    let out = bddkit(&bin, &["run", "--config", path(&cfg)]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{}", show(&out));
    assert!(stderr.contains("resources.db.z_dead"), "{stderr}");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("run "),
        "no run id is printed"
    );
}

#[test]
fn doctor_live_reports_every_dead_connection_by_name() {
    let bin = require_bddkit!();
    let cfg = default_db(
        project(
            "doctor-dsn",
            "  db:
    primary:
      dsn: postgres://u:p@127.0.0.1:1/x
    secondary:
      dsn: postgres://u:p@127.0.0.1:1/y
",
        ),
        "primary",
    );
    let out = bddkit(
        &bin,
        &["doctor", "--config", path(&cfg), "--live", "--json"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{}", show(&out));
    let report: Value = serde_json::from_str(&stdout).expect("--json emits JSON only");
    let checks = report["checks"].as_array().expect("checks is an array");
    for name in ["primary", "secondary"] {
        assert!(
            checks.iter().any(|c| c["stage"] == "db"
                && c["target"] == name
                && c["status"] == "failed"
                && c["probe"] == true),
            "the live probe of {name} must be reported failed:\n{stdout}"
        );
    }
}

/// A DSN is more than its scheme: whatever `run` refuses statically, a static
/// `doctor` must refuse too.
#[test]
fn doctor_and_run_agree_that_a_malformed_dsn_is_a_startup_failure() {
    let bin = require_bddkit!();
    let cfg = project(
        "baddsn",
        "  db:\n    primary:\n      dsn: \"postgres://u:p@127.0.0.1:notaport/x\"\n",
    );
    let doctor = bddkit(&bin, &["doctor", "--config", path(&cfg)]);
    let stdout = String::from_utf8_lossy(&doctor.stdout);
    assert_eq!(
        doctor.status.code(),
        Some(1),
        "no socket is needed\n{}",
        show(&doctor)
    );
    assert!(stdout.contains("db primary"), "{stdout}");

    let run = bddkit(&bin, &["run", "--config", path(&cfg)]);
    assert_eq!(run.status.code(), Some(2), "{}", show(&run));
}

/// A project whose connection has a password in its DSN and, in a field that
/// wants a list, the same URL again: a type error quotes the string it was
/// given, the one way to put the value into an error.
fn dsn_project(name: &str) -> PathBuf {
    let dsn = "postgres://app:dsn-pass-1234@127.0.0.1:1/app";
    project(
        name,
        &format!("  db:\n    main:\n      dsn: {dsn}\n      search_path: {dsn}\n"),
    )
}

#[test]
fn a_dsn_password_never_appears_in_a_connection_error() {
    let bin = require_bddkit!();
    let cfg = dsn_project("mask-run");
    let out = bddkit(&bin, &["run", "--config", path(&cfg)]);
    let text = show(&out);
    assert_eq!(out.status.code(), Some(2), "{text}");
    assert!(
        text.contains("expected a sequence"),
        "the error is shown: {text}"
    );
    assert!(text.contains("app:***@127.0.0.1"), "{text}");
    assert!(!text.contains("dsn-pass-1234"), "{text}");
}

#[test]
fn doctor_masks_a_dsn_password_in_its_report_and_its_json() {
    let bin = require_bddkit!();
    let cfg = dsn_project("mask-doctor");
    for extra in [&[][..], &["--json"][..]] {
        let mut args = vec!["doctor", "--config", path(&cfg)];
        args.extend_from_slice(extra);
        let out = bddkit(&bin, &args);
        let text = show(&out);
        assert_eq!(out.status.code(), Some(1), "{text}");
        assert!(text.contains("expected a sequence"), "{text}");
        assert!(text.contains("app:***@127.0.0.1"), "{text}");
        assert!(!text.contains("dsn-pass-1234"), "{text}");
    }
}

#[test]
fn doctor_shows_the_engine_statically_and_the_vendor_live_one_row_per_connection() {
    let bin = require_bddkit!();
    let mariadb = dsn(
        "BDDKIT_TEST_MARIADB_DSN",
        "mysql://root:root@127.0.0.1:3308/apibdd_it",
    );
    let mysql = dsn(
        "BDDKIT_TEST_MYSQL_DSN",
        "mysql://root:root@127.0.0.1:3307/apibdd_it",
    );
    let cfg = default_db(
        project(
            "rows",
            &format!("  db:\n    main:\n      dsn: {mysql}\n    legacy:\n      dsn: {mariadb}\n"),
        ),
        "main",
    );
    let stat = bddkit(&bin, &["doctor", "--config", path(&cfg), "--json"]);
    let stat: Value = serde_json::from_slice(&stat.stdout).expect("JSON");
    let detail = |report: &Value, target: &str, probe: bool| -> String {
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| {
                c["stage"] == "db"
                    && c["target"] == target
                    && c["probe"].as_bool().unwrap_or(false) == probe
            })
            .map(|c| c["detail"].to_string())
            .unwrap_or_default()
    };
    assert!(detail(&stat, "main", false).contains("mysql"), "{stat}");
    assert!(detail(&stat, "legacy", false).contains("mysql"), "{stat}");

    let live = bddkit(
        &bin,
        &["doctor", "--config", path(&cfg), "--live", "--json"],
    );
    let live: Value = serde_json::from_slice(&live.stdout).expect("JSON");
    assert!(
        detail(&live, "main", true).contains("connected, mysql"),
        "{live}"
    );
    assert!(
        detail(&live, "legacy", true).contains("connected, mariadb"),
        "{live}"
    );
}

#[test]
fn a_search_path_on_mysql_is_refused_at_load_and_an_empty_one_is_accepted() {
    let bin = require_bddkit!();
    let mysql = dsn(
        "BDDKIT_TEST_MYSQL_DSN",
        "mysql://root:root@127.0.0.1:3307/apibdd_it",
    );
    let refused = project(
        "sp-refused",
        &format!("  db:\n    main:\n      dsn: {mysql}\n      search_path: [s]\n"),
    );
    let out = bddkit(&bin, &["run", "--config", path(&refused)]);
    assert_eq!(out.status.code(), Some(2), "{}", show(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("search_path"),
        "{}",
        show(&out)
    );

    let accepted = project(
        "sp-empty",
        &format!("  db:\n    main:\n      dsn: {mysql}\n      search_path: []\n"),
    );
    let out = bddkit(&bin, &["run", "--config", path(&accepted)]);
    assert_eq!(out.status.code(), Some(0), "{}", show(&out));
}

#[test]
fn a_live_database_lets_the_run_start_and_pass() {
    let bin = require_bddkit!();
    let cfg = project(
        "a_live",
        &format!("  db:\n    main:\n      dsn: {}\n", postgres_dsn()),
    );
    let out = bddkit(&bin, &["run", "--config", path(&cfg)]);
    assert_eq!(out.status.code(), Some(0), "{}", show(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("run "),
        "{}",
        show(&out)
    );
}

/// The host generates `I use "<conn>" db` from the group name and the
/// deprecated `connection` word from the manifest, so old suites run as they
/// are, and `doctor` hints at the replacement without failing.
#[test]
fn the_host_generates_the_switch_steps_for_db_and_the_deprecated_connection() {
    let bin = require_bddkit!();
    let cfg = project(
        "switch",
        &format!("  db:\n    main:\n      dsn: {}\n", postgres_dsn()),
    );
    let feature = cfg.parent().unwrap().join("features/only.feature");
    std::fs::write(
        &feature,
        "Feature: only\n  Scenario: one\n    Given I use \"main\" db\n    And I use \"main\" connection\n",
    )
    .expect("feature");
    let run = bddkit(&bin, &["run", "--config", path(&cfg)]);
    assert_eq!(run.status.code(), Some(0), "{}", show(&run));
    let doctor = bddkit(&bin, &["doctor", "--config", path(&cfg)]);
    assert_eq!(
        doctor.status.code(),
        Some(0),
        "a hint never fails the row\n{}",
        show(&doctor)
    );
    assert!(show(&doctor).contains("connection"), "{}", show(&doctor));
}
