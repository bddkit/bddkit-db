use super::*;
use crate::sql::platform::{MARIADB, MYSQL, PG};

fn names(p: &InsertPlan) -> Vec<&str> {
    p.pk_vars.iter().map(|(n, _)| n.as_str()).collect()
}

#[expect(
    clippy::fn_params_excessive_bools,
    reason = "a test helper that mirrors Column's flags positionally"
)]
fn col(
    name: &str,
    ty: &str,
    not_null: bool,
    has_default: bool,
    is_identity: bool,
    is_pk: bool,
) -> Column {
    Column {
        name: name.into(),
        type_name: ty.into(),
        length: None,
        not_null,
        has_default,
        is_identity,
        is_pk,
    }
}

// users(id uuid pk with no default, email text not null, created_at timestamptz not null)
fn users_uuid() -> TableSchema {
    TableSchema {
        columns: vec![
            col("id", "uuid", true, false, false, true),
            col("email", "text", true, false, false, false),
            col("created_at", "timestamptz", true, false, false, false),
        ],
    }
}

#[test]
fn generates_uuid_pk_and_fills_timestamp() {
    let p = build_insert(
        &PG,
        &users_uuid(),
        "users",
        "users",
        &[("email".into(), Some("a@b.net".into()))],
        None,
    )
    .unwrap();
    // email is given ($1), id is generated ($2::uuid), created_at → now()
    assert!(
        p.sql.starts_with(
            "INSERT INTO users (email, id, created_at) VALUES ($1::text, $2::uuid, now())"
        ),
        "{}",
        p.sql
    );
    assert!(p.sql.ends_with("RETURNING (id)::text"), "{}", p.sql);
    assert_eq!(p.binds.len(), 2);
    assert_eq!(p.binds[0], Some("a@b.net".to_string()));
    assert!(uuid::Uuid::parse_str(p.binds[1].as_ref().unwrap()).is_ok());
    assert_eq!(names(&p), vec!["last_insert_id_users"]);
}

#[test]
fn omits_identity_and_default_pk() {
    // companies(id int identity pk, slug text not null)
    let s = TableSchema {
        columns: vec![
            col("id", "int4", true, true, true, true),
            col("slug", "text", true, false, false, false),
        ],
    };
    let p = build_insert(
        &PG,
        &s,
        "companies",
        "companies",
        &[("slug".into(), Some("x".into()))],
        None,
    )
    .unwrap();
    assert_eq!(
        p.sql,
        "INSERT INTO companies (slug) VALUES ($1::text) RETURNING (id)::text"
    );
    assert_eq!(names(&p), vec!["last_insert_id_companies"]);
}

#[test]
fn missing_plain_not_null_is_error() {
    let s = TableSchema {
        columns: vec![
            col("id", "int4", true, true, true, true),
            col("qty", "int4", true, false, false, false),
        ],
    };
    let err = build_insert(&PG, &s, "t", "t", &[], None).unwrap_err();
    assert!(err.message.contains("qty"), "{err}");
}

#[test]
fn unknown_column_is_error() {
    let err = build_insert(
        &PG,
        &users_uuid(),
        "users",
        "users",
        &[("nope".into(), Some("1".into()))],
        None,
    )
    .unwrap_err();
    assert!(err.message.contains("nope"), "{err}");
}

#[test]
fn provided_null_binds_none() {
    let s = TableSchema {
        columns: vec![
            col("id", "int4", true, true, true, true),
            col("deleted_at", "timestamptz", false, false, false, false),
        ],
    };
    let p = build_insert(&PG, &s, "t", "t", &[("deleted_at".into(), None)], None).unwrap();
    assert_eq!(p.binds, vec![None]);
    assert!(p.sql.contains("$1::timestamptz"), "{}", p.sql);
}

#[test]
fn composite_pk_yields_per_column_vars() {
    let s = TableSchema {
        columns: vec![
            col("a", "int4", true, false, false, true),
            col("b", "int4", true, false, false, true),
        ],
    };
    let p = build_insert(
        &PG,
        &s,
        "pair",
        "pair",
        &[
            ("a".into(), Some("1".into())),
            ("b".into(), Some("2".into())),
        ],
        None,
    )
    .unwrap();
    assert_eq!(names(&p), vec!["last_insert_pair_a", "last_insert_pair_b"]);
    assert!(
        p.sql.ends_with("RETURNING (a)::text, (b)::text"),
        "{}",
        p.sql
    );
}

#[test]
fn table_index_suffixes_var_name() {
    let p = build_insert(
        &PG,
        &users_uuid(),
        "users",
        "users",
        &[("email".into(), Some("a@b.net".into()))],
        Some(3),
    )
    .unwrap();
    assert_eq!(names(&p), vec!["last_insert_id_users_3"]);
}

fn companies() -> TableSchema {
    TableSchema {
        columns: vec![
            col("id", "int4", true, true, true, true),
            col("slug", "text", true, false, false, false),
            col("deleted_at", "timestamptz", false, false, false, false),
        ],
    }
}

#[test]
fn where_uses_typed_casts_and_is_null() {
    let (sql, binds) = build_where(
        &PG,
        &companies(),
        &[
            ("slug".into(), Some("x".into())),
            ("deleted_at".into(), None),
        ],
        1,
    )
    .unwrap();
    assert_eq!(sql, "slug = $1::text AND deleted_at IS NULL");
    assert_eq!(binds, vec![Some("x".to_string())]);
}

#[test]
fn where_param_numbering_respects_start() {
    let (sql, _) = build_where(&PG, &companies(), &[("slug".into(), Some("x".into()))], 4).unwrap();
    assert_eq!(sql, "slug = $4::text");
}

#[test]
fn where_unknown_column_is_error() {
    assert!(build_where(&PG, &companies(), &[("nope".into(), Some("1".into()))], 1).is_err());
}

#[test]
fn update_sets_then_where_numbering() {
    let (sql, binds) = build_update(
        &PG,
        &companies(),
        "companies",
        &[("slug".into(), Some("new".into()))],
        &[("id".into(), Some("7".into()))],
    )
    .unwrap();
    assert_eq!(
        sql,
        "UPDATE companies SET slug = $1::text WHERE id = $2::int4"
    );
    assert_eq!(binds, vec![Some("new".to_string()), Some("7".to_string())]);
}

#[test]
fn update_requires_where() {
    assert!(
        build_update(
            &PG,
            &companies(),
            "companies",
            &[("slug".into(), Some("x".into()))],
            &[]
        )
        .is_err()
    );
}

#[test]
fn operator_parsing_holds_at_its_edges() {
    // `parse_oneliner` trims the whole column, so `slug ~: x` arrives as
    // "slug ~" and the trim_end inside split_operator is what saves it.
    let (sql, _) = build_delete(
        &PG,
        &companies(),
        "companies",
        &[("slug ~".into(), Some("a%".into()))],
    )
    .unwrap();
    assert!(
        sql.contains("LIKE"),
        "a space before the operator is allowed: {sql}"
    );

    for raw in ["!", "~", "slug!!", "slug~!", "slug>="] {
        let err = build_where(&PG, &companies(), &[(raw.into(), Some("x".into()))], 1).unwrap_err();
        assert!(
            err.message.contains("unknown operator") && err.message.contains(raw),
            "{raw:?} must be refused by name, got: {err}"
        );
    }
}

#[test]
fn the_operator_comes_from_the_column_not_from_the_value() {
    // The whole point of the suffix: an unmarked column is `=` even when
    // the value is full of wildcards, so a `%` arriving through a variable
    // cannot widen a delete behind the author's back.
    let (sql, binds) = build_delete(
        &PG,
        &companies(),
        "companies",
        &[("slug".into(), Some("50%_x".into()))],
    )
    .unwrap();
    assert_eq!(sql, "DELETE FROM companies WHERE slug = $1::text");
    assert_eq!(binds, vec![Some("50%_x".to_string())]);
}

#[test]
fn a_tilde_asks_for_like_and_the_value_is_the_pattern() {
    let (sql, binds) = build_delete(
        &PG,
        &companies(),
        "companies",
        &[("slug~".into(), Some("demo_run_uabc123%".into()))],
    )
    .unwrap();
    assert_eq!(
        sql,
        "DELETE FROM companies WHERE (slug)::text LIKE $1::text"
    );
    assert_eq!(
        binds,
        vec![Some("demo_run_uabc123%".to_string())],
        "the author asked for a pattern: it reaches the server untouched"
    );
}

#[test]
fn bang_is_not_equal_and_bang_tilde_is_not_like() {
    let (sql, _) = build_delete(
        &PG,
        &companies(),
        "companies",
        &[
            ("id!".into(), Some("7".into())),
            ("slug!~".into(), Some("tmp%".into())),
        ],
    )
    .unwrap();
    assert_eq!(
        sql,
        "DELETE FROM companies WHERE id <> $1::int4 AND (slug)::text NOT LIKE $2::text"
    );
}

#[test]
fn null_reads_is_null_and_is_not_null() {
    let (sql, binds) = build_where(
        &PG,
        &companies(),
        &[("deleted_at".into(), None), ("slug!".into(), None)],
        1,
    )
    .unwrap();
    assert_eq!(sql, "deleted_at IS NULL AND slug IS NOT NULL");
    assert!(binds.is_empty(), "neither side binds anything");
}

#[test]
fn a_like_against_null_is_refused_naming_the_alternative() {
    let err = build_where(&PG, &companies(), &[("slug~".into(), None)], 1).unwrap_err();
    assert!(err.message.contains("slug"), "{err}");
    assert!(err.message.contains("<<null>>"), "{err}");
}

#[test]
fn an_unknown_operator_is_an_error_naming_it() {
    // Not "column \"price>=\" is missing from the table": that answer sends
    // the author to their DDL for what is a typo in the step.
    let err =
        build_where(&PG, &companies(), &[("slug>=".into(), Some("1".into()))], 1).unwrap_err();
    assert!(err.message.contains("slug>="), "{err}");
    assert!(err.message.contains("unknown operator"), "{err}");
}

#[test]
fn an_operator_is_refused_where_a_value_is_written() {
    let err = build_update(
        &PG,
        &companies(),
        "companies",
        &[("slug~".into(), Some("x%".into()))],
        &[("id".into(), Some("1".into()))],
    )
    .unwrap_err();
    assert!(err.message.contains("only allowed in a condition"), "{err}");
    let err = build_insert(
        &PG,
        &companies(),
        "companies",
        "companies",
        &[("slug!".into(), Some("x".into()))],
        None,
    )
    .unwrap_err();
    assert!(err.message.contains("only allowed in a condition"), "{err}");
}

#[test]
fn operators_reach_an_assertion_too() {
    // Nothing gates them by step any more: the syntax is explicit, so
    // there is no silent loosening left to protect an assertion from.
    let (sql, _) = build_exists(
        &PG,
        &companies(),
        "companies",
        &[("slug~".into(), Some("uabc%".into()))],
    )
    .unwrap();
    assert_eq!(
        sql,
        "SELECT 1 FROM companies WHERE (slug)::text LIKE $1::text LIMIT 1"
    );
}

#[test]
fn like_is_spelled_in_the_mysql_dialect() {
    let (sql, _) = build_delete(
        &MYSQL,
        &companies(),
        "companies",
        &[("slug~".into(), Some("uabc123%".into()))],
    )
    .unwrap();
    assert_eq!(sql, "DELETE FROM companies WHERE CAST(slug AS CHAR) LIKE ?");
}

#[test]
fn delete_requires_where() {
    assert!(build_delete(&PG, &companies(), "companies", &[]).is_err());
}

#[test]
fn delete_all_has_no_where() {
    assert_eq!(build_delete_all("companies"), "DELETE FROM companies");
}

#[test]
fn exists_selects_one() {
    let (sql, _) = build_exists(
        &PG,
        &companies(),
        "companies",
        &[("slug".into(), Some("x".into()))],
    )
    .unwrap();
    assert_eq!(sql, "SELECT 1 FROM companies WHERE slug = $1::text LIMIT 1");
}

// companies(id int auto_increment pk, slug text not null) — MySQL/MariaDB shape:
// is_identity true, has_default false (auto_increment is its own thing, not a
// DEFAULT in information_schema's sense).
fn mysql_companies() -> TableSchema {
    TableSchema {
        columns: vec![
            col("id", "int", true, false, true, true),
            col("slug", "text", true, false, false, false),
        ],
    }
}

#[test]
fn no_returning_and_no_value_reads_the_pk_off_auto_increment() {
    let p = build_insert(
        &MYSQL,
        &mysql_companies(),
        "companies",
        "companies",
        &[("slug".into(), Some("x".into()))],
        None,
    )
    .unwrap();
    assert!(!p.has_returning);
    assert_eq!(
        p.pk_vars,
        vec![("last_insert_id_companies".into(), PkSource::AutoIncrement)]
    );
}

#[test]
fn no_returning_and_a_given_value_is_known_even_on_an_identity_column() {
    let p = build_insert(
        &MYSQL,
        &mysql_companies(),
        "companies",
        "companies",
        &[
            ("id".into(), Some("5".into())),
            ("slug".into(), Some("x".into())),
        ],
        None,
    )
    .unwrap();
    assert_eq!(
        p.pk_vars,
        vec![(
            "last_insert_id_companies".into(),
            PkSource::Known("5".into())
        )]
    );
}

#[test]
fn no_returning_and_an_explicit_null_on_an_identity_column_is_still_auto_increment() {
    // <<null>> given for id: MySQL/MariaDB generate on NULL into
    // AUTO_INCREMENT exactly as they do when the column is omitted —
    // reported as Unknown here would be both the wrong diagnosis and
    // wrong advice ("give the value explicitly" on a column that just did).
    let p = build_insert(
        &MYSQL,
        &mysql_companies(),
        "companies",
        "companies",
        &[("id".into(), None), ("slug".into(), Some("x".into()))],
        None,
    )
    .unwrap();
    assert_eq!(
        p.pk_vars,
        vec![("last_insert_id_companies".into(), PkSource::AutoIncrement)]
    );
}

#[test]
fn no_returning_and_a_client_uuid_is_known_before_the_insert_runs() {
    // A lone char(36) PK with no default: MySQL's wants_client_uuid is
    // true for it, so build_insert generates the value up front — it is
    // Known, not AutoIncrement, even with no RETURNING to fall back on.
    let s = TableSchema {
        columns: vec![Column {
            name: "id".into(),
            type_name: "char".into(),
            length: Some(36),
            not_null: true,
            has_default: false,
            is_identity: false,
            is_pk: true,
        }],
    };
    let p = build_insert(&MYSQL, &s, "users", "users", &[], None).unwrap();
    assert!(!p.has_returning);
    let (name, source) = &p.pk_vars[0];
    assert_eq!(name, "last_insert_id_users");
    match source {
        PkSource::Known(id) => assert!(uuid::Uuid::parse_str(id).is_ok(), "{id}"),
        other => panic!("expected a client-generated uuid, got {other:?}"),
    }
}

#[test]
fn no_returning_and_a_non_identity_default_pk_is_refused_before_the_insert_runs() {
    // A server-side DEFAULT that is not AUTO_INCREMENT: has_default true,
    // is_identity false. MySQL has no RETURNING to read it back with, so
    // build_insert must refuse — before returning a plan, per invariant 1.
    let s = TableSchema {
        columns: vec![
            col("id", "int", true, true, false, true),
            col("tag", "text", true, false, false, false),
        ],
    };
    let err = build_insert(
        &MYSQL,
        &s,
        "defaulted",
        "defaulted",
        &[("tag".into(), Some("x".into()))],
        None,
    )
    .unwrap_err();
    assert!(err.message.contains("id"), "{err}");
    assert!(err.message.contains("server-generated"), "{err}");
}

#[test]
fn the_same_non_identity_default_pk_is_fine_under_returning() {
    // Identical schema, MariaDB instead: RETURNING exists, so the same
    // "no source" shape that MySQL must refuse is simply read back.
    let s = TableSchema {
        columns: vec![
            col("id", "int", true, true, false, true),
            col("tag", "text", true, false, false, false),
        ],
    };
    let p = build_insert(
        &MARIADB,
        &s,
        "defaulted",
        "defaulted",
        &[("tag".into(), Some("x".into()))],
        None,
    )
    .unwrap();
    assert!(p.has_returning);
}
