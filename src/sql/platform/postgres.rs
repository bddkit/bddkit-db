use super::Platform;
use crate::sql::plan::Column;
use crate::sql::reference::TableRef;

/// A single query against `pg_catalog`. `to_regclass($1)` resolves the name
/// honoring search_path; an empty result means "table not found".
const INTROSPECT_SQL: &str = "\
SELECT a.attname::text AS name,
       t.typname::text AS type_name,
       NULL::int AS length,
       a.attnotnull::int AS not_null,
       (a.atthasdef OR a.attidentity <> '')::int AS has_default,
       (a.attidentity <> '')::int AS is_identity,
       EXISTS (
         SELECT 1 FROM pg_constraint c
         WHERE c.conrelid = a.attrelid AND c.contype = 'p' AND a.attnum = ANY (c.conkey)
       )::int AS is_pk
FROM pg_attribute a
JOIN pg_type t ON t.oid = a.atttypid
WHERE a.attrelid = to_regclass($1) AND a.attnum > 0 AND NOT a.attisdropped
ORDER BY a.attnum";

pub struct Postgres;

pub static PG: Postgres = Postgres;

impl Platform for Postgres {
    fn name(&self) -> &'static str {
        "postgres"
    }

    fn bind(&self, n: usize, ty: &str) -> String {
        format!("${n}::{ty}")
    }

    fn placeholder(&self, n: usize) -> String {
        format!("${n}")
    }

    fn cast_text(&self, expr: &str) -> String {
        format!("({expr})::text")
    }

    fn insert_no_columns(&self, table: &str) -> String {
        format!("INSERT INTO {table} DEFAULT VALUES")
    }

    fn returning(&self, pk: &[&Column]) -> Option<String> {
        if pk.is_empty() {
            return None;
        }
        let cols: Vec<String> = pk.iter().map(|c| self.cast_text(&c.name)).collect();
        Some(format!("RETURNING {}", cols.join(", ")))
    }

    fn is_timestamplike(&self, ty: &str) -> bool {
        matches!(ty, "timestamp" | "timestamptz" | "date")
    }

    fn wants_client_uuid(&self, col: &Column) -> bool {
        col.type_name == "uuid"
    }

    fn check_bindable(&self, _col: &Column) -> Result<(), String> {
        Ok(())
    }

    fn introspect(&self, tref: &TableRef) -> (String, Vec<Option<String>>) {
        (INTROSPECT_SQL.to_string(), vec![Some(tref.sql_name())])
    }

    fn next_sequence(&self, seq: &str) -> Option<(String, Vec<Option<String>>)> {
        Some((
            "SELECT nextval($1::regclass)::text".to_string(),
            vec![Some(seq.to_string())],
        ))
    }

    fn session_setup(&self, search_path: &[String]) -> Result<Vec<String>, String> {
        if search_path.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![format!(
            "SET search_path TO {}",
            search_path.join(", ")
        )])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: &str) -> Column {
        Column {
            name: name.into(),
            type_name: ty.into(),
            length: None,
            not_null: false,
            has_default: false,
            is_identity: false,
            is_pk: true,
        }
    }

    #[test]
    fn dialect_produces_postgres_syntax() {
        let p = &PG;
        assert_eq!(p.bind(1, "int4"), "$1::int4");
        assert_eq!(p.placeholder(3), "$3");
        assert_eq!(p.cast_text("id"), "(id)::text");
        assert_eq!(p.insert_no_columns("t"), "INSERT INTO t DEFAULT VALUES");
        assert!(p.is_timestamplike("timestamptz"));
        assert!(!p.is_timestamplike("int4"));
        assert_eq!(
            p.next_sequence("s"),
            Some((
                "SELECT nextval($1::regclass)::text".to_string(),
                vec![Some("s".to_string())]
            ))
        );
    }

    #[test]
    fn returning_is_none_for_an_empty_pk_and_a_clause_for_a_real_one() {
        let p = &PG;
        assert_eq!(p.returning(&[]), None);
        let id = col("id", "int4");
        assert_eq!(
            p.returning(&[&id]),
            Some("RETURNING (id)::text".to_string())
        );
        let a = col("a", "int4");
        let b = col("b", "int4");
        assert_eq!(
            p.returning(&[&a, &b]),
            Some("RETURNING (a)::text, (b)::text".to_string())
        );
    }

    #[test]
    fn session_setup_is_empty_with_no_search_path() {
        let p = &PG;
        assert_eq!(p.session_setup(&[]).unwrap(), Vec::<String>::new());
        assert_eq!(
            p.session_setup(&["a".into(), "b".into()]).unwrap(),
            vec!["SET search_path TO a, b".to_string()]
        );
    }
}
