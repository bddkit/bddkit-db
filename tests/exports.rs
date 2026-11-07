//! The C exports driven in-process, the way the host drives them: JSON in,
//! JSON out, every returned string handed back to `bddkit_free_string`.
//! Needs no `bddkit` binary. The live tests need the databases CI starts, at
//! the addresses of `DSNS` unless `BDDKIT_TEST_DSN`, `BDDKIT_TEST_MYSQL_DSN`
//! and `BDDKIT_TEST_MARIADB_DSN` say otherwise.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "`allow-*-in-tests` covers only `#[test]` functions, not the helpers here"
)]

use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char};

use bddkit_db::db::Db;
use bddkit_db::engine::Engine;
use bddkit_db::*;
use serde_json::{Value, json};

/// Engine, the variable that overrides its DSN, and the DSN of the dev stack.
const DSNS: [(Engine, &str, &str); 3] = [
    (
        Engine::Postgres,
        "BDDKIT_TEST_DSN",
        "postgres://postgres:postgres@127.0.0.1:5433/postgres",
    ),
    (
        Engine::MySql,
        "BDDKIT_TEST_MYSQL_DSN",
        "mysql://root:root@127.0.0.1:3307/apibdd_it",
    ),
    (
        Engine::MariaDb,
        "BDDKIT_TEST_MARIADB_DSN",
        "mysql://root:root@127.0.0.1:3308/apibdd_it",
    ),
];

fn dsn(variable: &str, default: &str) -> String {
    std::env::var(variable).unwrap_or_else(|_| default.to_string())
}

fn call(reply: *mut c_char) -> Value {
    assert!(!reply.is_null(), "an export returned NULL");
    // SAFETY: not null (asserted above), and every reply of this library comes
    // from `CString::into_raw`, so it is NUL-terminated and lives until it is
    // freed below.
    let text = unsafe { CStr::from_ptr(reply) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: an export returned it, and this is the one place it is freed.
    unsafe { bddkit_free_string(reply) };
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

fn cstr(v: &Value) -> CString {
    CString::new(v.to_string()).expect("no NUL")
}

fn instance_request(config: &Value) -> CString {
    cstr(&json!({"group": "db", "instance": "t", "config": config, "options": {}}))
}

fn validate(config: &Value) -> Value {
    call(bddkit_validate_config(instance_request(config).as_ptr()))
}

#[test]
fn the_abi_and_the_step_list_are_what_the_plugin_promises() {
    assert_eq!(bddkit_abi_version(), 4);
    let steps = call(bddkit_list_steps());
    let steps = steps.as_array().expect("an array");
    assert_eq!(steps.len(), 14);
    let mut expressions = Vec::new();
    for step in steps {
        assert_eq!(step["group"], "db");
        // A step that declares no effect is unknown to `doctor`.
        assert!(step["sets_vars"].is_array(), "{step}");
        assert!(step["description"].as_str().is_some_and(|d| !d.is_empty()));
        assert!(step.get("pattern").is_none(), "{step}");
        expressions.push(step["expression"].as_str().expect("expression"));
    }
    expressions.sort_unstable();
    expressions.dedup();
    assert_eq!(expressions.len(), 14, "one expression per step");
    let assertions: Vec<_> = steps.iter().filter(|s| s["kind"] == "assertion").collect();
    assert_eq!(assertions.len(), 4);
    assert!(assertions.iter().all(|s| {
        s["expression"]
            .as_str()
            .is_some_and(|e| e.contains("should"))
    }));
}

/// `bddkit_validate_step` for the step whose expression starts with `prefix`.
fn validate_step(prefix: &str, args: &[&str]) -> Value {
    let steps = call(bddkit_list_steps());
    let index = steps
        .as_array()
        .expect("an array")
        .iter()
        .position(|s| {
            s["expression"]
                .as_str()
                .is_some_and(|e| e.starts_with(prefix))
        })
        .expect("a step with this expression");
    let request = cstr(&json!({
        "args": args, "docstring": null, "table": null, "source_dir": "/x"
    }));
    call(bddkit_validate_step(
        u32::try_from(index).expect("small"),
        request.as_ptr(),
    ))
}

#[test]
fn validate_step_refuses_what_fails_at_the_step_for_sure() {
    let insert = r#"I have "{table}" with"#;
    for (prefix, args, why) in [
        (insert, ["t", "no colon here"], "a pair without a colon"),
        (
            insert,
            ["t", "slug~: x"],
            "an operator where a value is written",
        ),
        (
            r#"I delete "{table}" where"#,
            ["t", "price>=: 3"],
            "an unknown operator",
        ),
    ] {
        let v = validate_step(prefix, &args);
        assert_eq!(v["ok"], false, "{why}: {v}");
    }
    let v = validate_step("I call procedure", &["p", "a:1,oops"]);
    assert_eq!(v["ok"], false, "{v}");
}

#[test]
fn validate_step_accepts_a_slot_anywhere_and_a_correct_literal() {
    let insert = r#"I have "{table}" with"#;
    assert_eq!(validate_step(insert, &["t", "slug: ok, n: 2"])["ok"], true);
    // A slot may fill in anything, so the argument is not read at all.
    assert_eq!(validate_step(insert, &["t", "<<pairs>>"])["ok"], true);
    assert_eq!(validate_step(insert, &["t", "slug~: <<x>>"])["ok"], true);
    // An operator is right in a condition.
    let delete = r#"I delete "{table}" where"#;
    for condition in ["a!: 1", "a~: x%", "a!~: y", "a: <<null>>"] {
        assert_eq!(
            validate_step(delete, &["t", condition])["ok"],
            true,
            "{condition}"
        );
    }
    // The column of an extract takes no operator.
    let extract = "I extract";
    assert_eq!(
        validate_step(extract, &["id", "t", "a: 1", "v"])["ok"],
        true
    );
    assert_eq!(
        validate_step(extract, &["id~", "t", "a: 1", "v"])["ok"],
        false
    );
}

#[test]
fn validate_config_reports_the_engine_as_the_detail() {
    let v = validate(&json!({"dsn": "postgres://u:p@127.0.0.1:5432/x"}));
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["detail"], "postgres");
    let v = validate(&json!({"dsn": "mariadb://u:p@127.0.0.1:3306/x"}));
    assert_eq!(v["detail"], "mysql", "{v}");
}

#[test]
fn validate_config_refuses_what_the_run_would_refuse_offline() {
    for config in [
        json!({}),
        json!({"dsn": "sqlite://x"}),
        json!({"dsn": "postgres://u:p@127.0.0.1:notaport/x"}),
        json!({"dsn": "postgres://u:p@h/x", "search_path": "public"}),
    ] {
        let v = validate(&config);
        assert_eq!(v["ok"], false, "{config}");
        assert!(v["error"].as_str().is_some_and(|e| !e.is_empty()), "{v}");
    }
}

#[test]
fn a_search_path_is_refused_on_the_mysql_family_and_accepted_when_empty() {
    let refused = validate(&json!({"dsn": "mysql://u:p@h/x", "search_path": ["s"]}));
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        refused["error"].as_str().unwrap().contains("search_path"),
        "{refused}"
    );
    let accepted = validate(&json!({"dsn": "mysql://u:p@h/x", "search_path": []}));
    assert_eq!(accepted["ok"], true, "{accepted}");
    let postgres = validate(&json!({"dsn": "postgres://u:p@h/x", "search_path": ["a", "b"]}));
    assert_eq!(postgres["ok"], true, "{postgres}");
}

#[test]
fn probe_config_connects_and_names_the_vendor() {
    for (engine, variable, default) in DSNS {
        let config = json!({"dsn": dsn(variable, default)});
        let v = call(bddkit_probe_config(instance_request(&config).as_ptr()));
        assert_eq!(v["ok"], true, "{engine:?}: {v}");
        assert_eq!(v["detail"], format!("connected, {}", engine.name()));
    }
}

#[test]
fn init_opens_every_connection_once_and_drop_closes_them() {
    let instances: serde_json::Map<String, Value> = DSNS
        .iter()
        .map(|(engine, variable, default)| {
            (
                engine.name().to_string(),
                json!({"config": {"dsn": dsn(variable, default)}, "options": {}}),
            )
        })
        .collect();
    let request = cstr(&json!({"group": "db", "instances": instances, "concurrency": 4}));
    let first = call(bddkit_init_instance(request.as_ptr()));
    let second = call(bddkit_init_instance(request.as_ptr()));
    assert_eq!(first["ok"], true, "{first}");
    assert_ne!(first["handle"], second["handle"], "handles are distinct");
    for reply in [&first, &second] {
        let v = call(bddkit_drop_instance(reply["handle"].as_u64().unwrap()));
        assert_eq!(v["ok"], true, "{v}");
    }
    let v = call(bddkit_drop_instance(first["handle"].as_u64().unwrap()));
    assert_eq!(v["ok"], false, "a handle is dropped once");
}

#[test]
fn init_refuses_an_unreadable_config_naming_the_instance() {
    let request = cstr(&json!({
        "group": "db",
        "instances": {"audit": {"config": {"dsn": "sqlite://x"}, "options": {}}},
        "concurrency": 1
    }));
    let v = call(bddkit_init_instance(request.as_ptr()));
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["instance"], "audit");
}

#[tokio::test]
async fn a_connection_is_told_from_its_vendor_by_a_query() {
    for (engine, variable, default) in DSNS {
        let connection =
            config_for(&dsn(variable, default)).unwrap_or_else(|e| panic!("{engine:?}: {e}"));
        // A pool size of 0 is raised to 1, not refused.
        let db = Db::connect(&BTreeMap::from([("c".to_string(), connection)]), 0)
            .await
            .unwrap_or_else(|e| panic!("{engine:?}: {} {}", e.connection, e.message));
        assert_eq!(db.engine("c"), Some(engine));
        assert_eq!(db.engine("other"), None);
        db.close().await;
    }
}

fn config_for(dsn: &str) -> Result<bddkit_db::config::Connection, String> {
    bddkit_db::config::Connection::parse(&json!({"dsn": dsn}))
}
