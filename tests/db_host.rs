//! What the steps do beside the host: several files on one shared handle,
//! evidence, the failure dump, the static check of a macro body and the
//! `doctor` hint for the old switch word. Postgres by default, any engine
//! through `BDDKIT_TEST_ENGINE`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "`allow-*-in-tests` covers only `#[test]` functions, not the helpers here"
)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU32, Ordering};

use common::db::{connection_yaml, setup};
use common::{combined, command};

/// A suite on disk: feature files, an optional macro file and the config.
struct Suite {
    dir: PathBuf,
    config: PathBuf,
}

impl Suite {
    /// `connections` are the names under `resources.db`; the first is the default.
    fn new(features: &[(&str, &str)], connections: &[&str]) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "bddkit-db-host-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(dir.join("features")).expect("mkdir");
        for (name, text) in features {
            std::fs::write(dir.join("features").join(name), text).expect("feature");
        }
        let resources: String = connections.iter().map(|c| connection_yaml(c)).collect();
        let config = dir.join("cfg.yaml");
        std::fs::write(
            &config,
            format!(
                "paths: [{}]\ndefault_db: {}\nresources:\n  db:\n{resources}",
                dir.join("features")
                    .display()
                    .to_string()
                    .replace('\\', "/"),
                connections[0]
            ),
        )
        .expect("config");
        Self { dir, config }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn bddkit(&self, subcommand: &str, extra: &[&str]) -> Output {
        let bin = common::bddkit_bin().expect("callers skip first, with require_bddkit!");
        let config = self.config.to_str().expect("UTF-8");
        let mut args = vec![subcommand, "--config", config];
        args.extend_from_slice(extra);
        command(&bin, &args).output().expect("bddkit")
    }

    fn run(&self, extra: &[&str]) -> Output {
        self.bddkit("run", extra)
    }
}

fn utf8(p: &Path) -> &str {
    p.to_str().expect("UTF-8")
}

/// Each file inserts its own row, then waits for the other's: the pair can
/// only pass if both run at once. Nothing but the plugin's own steps is the
/// barrier, so it holds for any host.
fn barrier_file(mine: &str, theirs: &str) -> String {
    format!(
        "Feature: barrier {mine}
  Scenario: meet
    Given I have \"companies\" with \"slug: {mine}\"
    And I expect the next assertion to pass within \"3\" seconds, checking every \"50\" milliseconds
    Then I should have \"companies\" with \"slug: {theirs}\"
"
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_files_run_statements_on_the_shared_handle_at_the_same_time() {
    require_bddkit!();
    let guard = setup().await;
    let suite = Suite::new(
        &[
            ("a.feature", &barrier_file("barrier-a", "barrier-b")),
            ("b.feature", &barrier_file("barrier-b", "barrier-a")),
        ],
        &["default"],
    );
    let out = suite.run(&["--concurrency", "2"]);
    assert!(out.status.success(), "{}", combined(&out));
    drop(guard);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_single_worker_cannot_release_the_barrier() {
    require_bddkit!();
    let guard = setup().await;
    let suite = Suite::new(
        &[
            ("a.feature", &barrier_file("barrier-a", "barrier-b")),
            ("b.feature", &barrier_file("barrier-b", "barrier-a")),
        ],
        &["default"],
    );
    // The first file waits for a row the second has not inserted, because
    // the second has not started: the control that the pair above proves
    // concurrency and not luck.
    let out = suite.run(&["--concurrency", "1"]);
    assert_eq!(out.status.code(), Some(1), "{}", combined(&out));
    drop(guard);
}

fn events(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .expect("the stream exists")
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_statement_is_evidence_of_the_connection_the_reference_names() {
    require_bddkit!();
    let guard = setup().await;
    let suite = Suite::new(
        &[(
            "db.feature",
            "Feature: evidence
  Scenario: a reference names a connection
    Given I have \"companies\" with \"slug: on-default\"
    And I have \"other:companies\" with \"slug: on-other\"
",
        )],
        &["default", "other"],
    );
    let stream = suite.path("events.ndjson");
    let out = suite.run(&["--events", utf8(&stream), "--evidence"]);
    assert!(out.status.success(), "{}", combined(&out));
    let sql: Vec<_> = events(&stream)
        .into_iter()
        .filter(|e| e["type"] == "evidence" && e["kind"] == "sql")
        .collect();
    let resources: Vec<_> = sql.iter().map(|e| e["resource"].clone()).collect();
    assert_eq!(resources, ["default", "other"], "{sql:?}");
    drop(guard);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_step_dump_carries_no_sql_and_the_stream_does_under_evidence() {
    require_bddkit!();
    let guard = setup().await;
    let suite = Suite::new(
        &[(
            "db.feature",
            "Feature: failure
  Scenario: the server refuses
    Given I have \"companies\" with \"slug: <<null>>\"
",
        )],
        &["default"],
    );
    // The built statement, not the driver's own words about it.
    let built = "INSERT INTO";
    let plain = suite.run(&[]);
    assert_eq!(plain.status.code(), Some(1), "{}", combined(&plain));
    assert!(!combined(&plain).contains(built), "{}", combined(&plain));

    let stream = suite.path("events.ndjson");
    let with = suite.run(&["--events", utf8(&stream), "--evidence"]);
    assert_eq!(with.status.code(), Some(1), "{}", combined(&with));
    assert!(!combined(&with).contains(built), "{}", combined(&with));
    let sql: Vec<_> = events(&stream)
        .into_iter()
        .filter(|e| e["type"] == "evidence" && e["kind"] == "sql")
        .collect();
    assert_eq!(sql.len(), 1, "{sql:?}");
    assert!(
        sql[0]["content"]["sql"]
            .as_str()
            .is_some_and(|s| s.starts_with(built)),
        "{sql:?}"
    );
    drop(guard);
}

/// A macro file with one macro nobody calls, whose body is `step`.
fn uncalled_macro(step: &str) -> Suite {
    let suite = Suite::new(
        &[(
            "only.feature",
            "Feature: only\n  Scenario: one\n    Given set variable \"x\" to \"1\"\n",
        )],
        &["default"],
    );
    std::fs::write(
        suite.path("macros.yaml"),
        format!("- step: 'I never call this'\n  do:\n    - {step}\n"),
    )
    .expect("macros");
    suite
}

#[test]
fn a_literal_error_in_a_macro_body_nobody_calls_is_exit_two_and_a_slot_is_not() {
    require_bddkit!();
    let broken = uncalled_macro(r#"I have "companies" with "no colon here""#);
    let macros = broken.path("macros.yaml");
    let out = broken.run(&["--macro-paths", utf8(&macros)]);
    assert_eq!(out.status.code(), Some(2), "{}", combined(&out));
    assert!(
        combined(&out).contains("never call this"),
        "{}",
        combined(&out)
    );

    // A slot may fill in a valid value, so the same place is not read.
    let slotted = uncalled_macro(r#"I have "companies" with "<<pairs>>""#);
    let macros = slotted.path("macros.yaml");
    let out = slotted.run(&["--macro-paths", utf8(&macros)]);
    assert!(out.status.success(), "{}", combined(&out));
}

#[test]
fn doctor_hints_at_the_old_switch_word_and_run_is_silent() {
    require_bddkit!();
    let suite = Suite::new(
        &[(
            "db.feature",
            "Feature: switch
  Scenario: the old word
    Given I use \"default\" connection
",
        )],
        &["default"],
    );
    let doctor = suite.bddkit("doctor", &[]);
    assert!(doctor.status.success(), "{}", combined(&doctor));
    assert!(
        combined(&doctor)
            .contains(r#"I use "default" connection is deprecated, write I use "default" db"#),
        "{}",
        combined(&doctor)
    );
    let run = suite.run(&[]);
    assert!(run.status.success(), "{}", combined(&run));
    assert!(!combined(&run).contains("deprecated"), "{}", combined(&run));
}
