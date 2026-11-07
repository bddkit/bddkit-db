//! The SQL layer against the three real engines: the NULL sentinel reaches the
//! database as NULL, and the schema cache keeps what it read first. Needs the
//! databases CI starts, at the addresses of `DSNS` unless `BDDKIT_TEST_DSN`,
//! `BDDKIT_TEST_MYSQL_DSN` and `BDDKIT_TEST_MARIADB_DSN` say otherwise.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "`allow-*-in-tests` covers only `#[test]` functions, not the helpers here"
)]

use std::collections::BTreeMap;
use std::sync::Arc;

use bddkit_db::config::Connection;
use bddkit_db::db::Db;
use bddkit_db::engine::{Engine, install_drivers};
use bddkit_db::error::Class;
use bddkit_db::sql::bind_all;
use bddkit_db::sql::plan::{build_exists, build_insert};
use bddkit_db::sql::reference::TableRef;
use bddkit_db::sql::value::{NULL_SENTINEL, parse_oneliner};
use sqlx::AnyPool;

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

/// One engine's pool for DDL and statements, and a `Db` over the same DSN.
async fn open(variable: &str, default: &str) -> (AnyPool, Db) {
    install_drivers();
    let dsn = std::env::var(variable).unwrap_or_else(|_| default.to_string());
    let pool = AnyPool::connect(&dsn).await.expect("connect");
    let connections = BTreeMap::from([(
        "t".to_string(),
        Connection {
            dsn,
            search_path: Vec::new(),
        },
    )]);
    let db = Db::connect(&connections, 2).await.expect("Db::connect");
    (pool, db)
}

async fn run(pool: &AnyPool, sql: &str) {
    sqlx::query(sql).execute(pool).await.expect(sql);
}

#[tokio::test]
async fn the_null_sentinel_is_sql_null_on_every_engine() {
    for (engine, variable, default) in DSNS {
        let (pool, db) = open(variable, default).await;
        let platform = engine.platform();
        let table = format!("bddkit_sql_null_{}", std::process::id());
        run(&pool, &format!("DROP TABLE IF EXISTS {table}")).await;
        run(
            &pool,
            &format!("CREATE TABLE {table} (id int PRIMARY KEY, note text)"),
        )
        .await;

        let tref = TableRef::parse(&table).expect("ref");
        let schema = db.schema("t", &tref).await.expect("schema");
        let given = parse_oneliner(&format!("id: 1, note: {NULL_SENTINEL}")).expect("pairs");
        assert_eq!(given[1].1, None, "{engine:?}: the sentinel parses to NULL");
        let insert = build_insert(platform, &schema, &table, &table, &given, None).expect("plan");
        bind_all(sqlx::query(&insert.sql), &insert.binds)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("{engine:?}: {}: {e}", insert.sql));

        for (condition, matches) in [
            (format!("note: {NULL_SENTINEL}"), true),
            (format!("note!: {NULL_SENTINEL}"), false),
        ] {
            let pairs = parse_oneliner(&condition).expect("pairs");
            let (sql, binds) = build_exists(platform, &schema, &table, &pairs).expect("exists");
            assert!(sql.contains("IS "), "{engine:?}: {sql}");
            assert!(binds.is_empty(), "{engine:?}: {sql} binds {binds:?}");
            let row = bind_all(sqlx::query(&sql), &binds)
                .fetch_optional(&pool)
                .await
                .unwrap_or_else(|e| panic!("{engine:?}: {sql}: {e}"));
            assert_eq!(row.is_some(), matches, "{engine:?}: {condition}");
        }
        run(&pool, &format!("DROP TABLE {table}")).await;
    }
}

#[tokio::test]
async fn a_schema_is_read_once_and_kept_for_the_run() {
    for (engine, variable, default) in DSNS {
        let (pool, db) = open(variable, default).await;
        let table = format!("bddkit_sql_cache_{}", std::process::id());
        run(&pool, &format!("DROP TABLE IF EXISTS {table}")).await;
        run(&pool, &format!("CREATE TABLE {table} (id int PRIMARY KEY)")).await;

        let tref = TableRef::parse(&table).expect("ref");
        let first = db.schema("t", &tref).await.expect("schema");
        run(&pool, &format!("ALTER TABLE {table} ADD COLUMN extra int")).await;
        let second = db.schema("t", &tref).await.expect("schema");

        assert!(Arc::ptr_eq(&first, &second), "{engine:?}: read again");
        assert!(
            second.col("extra").is_none(),
            "{engine:?}: DDL became visible"
        );
        run(&pool, &format!("DROP TABLE {table}")).await;
    }
}

#[tokio::test]
async fn an_undeclared_connection_is_an_input_error_naming_it() {
    let (variable, default) = (DSNS[0].1, DSNS[0].2);
    let (_pool, db) = open(variable, default).await;
    let tref = TableRef::parse("anything").expect("ref");
    let error = db.schema("nope", &tref).await.expect_err("undeclared");
    assert_eq!(error.class, Class::Input);
    assert_eq!(
        error.message,
        r#"connection "nope" is not declared in resources.db"#
    );
}
