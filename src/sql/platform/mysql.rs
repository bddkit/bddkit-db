use super::Platform;
use crate::sql::plan::Column;
use crate::sql::reference::TableRef;

/// `information_schema.columns` for one table. `COALESCE(?, DATABASE())` lets
/// an unqualified `TableRef` mean "the connection's own database" — MySQL has
/// no session search_path, a schema IS a database. `DATA_TYPE` (not
/// `COLUMN_TYPE`) gives the bare type name (`varchar`, not `varchar(36)`); the
/// length is read separately into the `length` column.
///
/// `has_default` folds in two engine quirks, both confirmed against live
/// servers, not just the doc: MariaDB stores the four-character string
/// `'NULL'` in `COLUMN_DEFAULT` for a nullable column with no real default
/// (MySQL stores an actual SQL NULL there), and a `STORED GENERATED` column
/// on MySQL reports `EXTRA LIKE '%GENERATED'` with `COLUMN_DEFAULT` NULL and
/// `IS_NULLABLE = 'NO'` — Postgres's `atthasdef` is true for a generated
/// column and `build_insert` relies on that to skip it, so this must match.
const INTROSPECT_SQL: &str = "\
SELECT COLUMN_NAME AS name,
       DATA_TYPE AS type_name,
       CHARACTER_MAXIMUM_LENGTH AS length,
       (IS_NULLABLE = 'NO') AS not_null,
       ((COLUMN_DEFAULT IS NOT NULL AND COLUMN_DEFAULT <> 'NULL') OR EXTRA LIKE '%GENERATED') AS has_default,
       (EXTRA = 'auto_increment') AS is_identity,
       (COLUMN_KEY = 'PRI') AS is_pk
FROM information_schema.columns
WHERE TABLE_SCHEMA = COALESCE(?, DATABASE()) AND TABLE_NAME = ?
ORDER BY ORDINAL_POSITION";

pub struct MySql {
    name: &'static str,
    returning: bool,
    sequences: bool,
    native_uuid: bool,
}

pub static MYSQL: MySql = MySql {
    name: "mysql",
    returning: false,
    sequences: false,
    native_uuid: false,
};

pub static MARIADB: MySql = MySql {
    name: "mariadb",
    returning: true,
    sequences: true,
    native_uuid: true,
};

impl Platform for MySql {
    fn name(&self) -> &'static str {
        self.name
    }

    fn bind(&self, _n: usize, _ty: &str) -> String {
        // MySQL has no `$N::type` cast syntax; a text bind is coerced
        // implicitly, so the column type is unused here on purpose.
        "?".to_string()
    }

    fn placeholder(&self, _n: usize) -> String {
        "?".to_string()
    }

    fn cast_text(&self, expr: &str) -> String {
        // Deliberately no width: `CAST(x AS CHAR(N))` decodes cleanly through
        // sqlx::Any, but a fixed N trades a decode failure for a silent
        // truncation — a truncated value comparing equal to nothing is the
        // green-test-over-wrong-data failure this design refuses elsewhere.
        // The unwidened CAST can still land as a blob on the wire; that is
        // handled once, at the decode boundary (`db::text_col`), not here.
        format!("CAST({expr} AS CHAR)")
    }

    fn insert_no_columns(&self, table: &str) -> String {
        format!("INSERT INTO {table} () VALUES ()")
    }

    fn returning(&self, pk: &[&Column]) -> Option<String> {
        if !self.returning || pk.is_empty() {
            return None;
        }
        let cols: Vec<String> = pk.iter().map(|c| self.cast_text(&c.name)).collect();
        Some(format!("RETURNING {}", cols.join(", ")))
    }

    fn is_timestamplike(&self, ty: &str) -> bool {
        matches!(ty, "timestamp" | "datetime" | "date")
    }

    fn wants_client_uuid(&self, col: &Column) -> bool {
        if self.native_uuid && col.type_name == "uuid" {
            return true;
        }
        matches!(col.type_name.as_str(), "char" | "varchar") && col.length == Some(36)
    }

    fn check_bindable(&self, col: &Column) -> Result<(), String> {
        if !matches!(col.type_name.as_str(), "binary" | "varbinary") {
            return Ok(());
        }
        let mut msg = format!(
            "column {} ({}) is a binary column; this layer binds and compares everything as \
             text, so a value here would be the ASCII bytes of what the .feature spells — an \
             INSERT can silently store the wrong bytes and a WHERE against it matches nothing",
            col.name, col.type_name
        );
        if col.length == Some(16) {
            msg.push_str(
                ". UUID_TO_BIN(uuid, swap_flag) takes a flag that reorders time-low and \
                 time-high, BIN_TO_UUID must be given the same flag, and information_schema \
                 does not record which flag the application chose — bddkit refuses to guess \
                 and write disagreeing bytes. Store the UUID as char(36) instead, or keep this \
                 column out of the step",
            );
        } else {
            msg.push_str(
                ". Keep this column out of the step, or expose it through a text/hex column \
                 or view instead",
            );
        }
        Err(msg)
    }

    fn introspect(&self, tref: &TableRef) -> (String, Vec<Option<String>>) {
        (
            INTROSPECT_SQL.to_string(),
            vec![tref.schema.clone(), Some(tref.table.clone())],
        )
    }

    fn next_sequence(&self, seq: &str) -> Option<(String, Vec<Option<String>>)> {
        if !self.sequences {
            return None;
        }
        // MariaDB cannot bind an identifier; the name is interpolated the
        // same way TableRef::sql_name() already interpolates table/schema.
        // NEXTVAL(...) returns BIGINT, which sqlx::Any's String decode
        // rejects outright — cast_text routes it through the same
        // blob-tolerant read path as extract/call_function.
        Some((
            format!("SELECT {}", self.cast_text(&format!("NEXTVAL({seq})"))),
            Vec::new(),
        ))
    }

    fn session_setup(&self, search_path: &[String]) -> Result<Vec<String>, String> {
        if search_path.is_empty() {
            return Ok(Vec::new());
        }
        Err(format!(
            "{}: a schema is a database, there is no session-level search_path; \
             qualify each table reference as \"schema.table\" instead of setting search_path",
            self.name()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: &str, length: Option<i64>) -> Column {
        Column {
            name: name.into(),
            type_name: ty.into(),
            length,
            not_null: false,
            has_default: false,
            is_identity: false,
            is_pk: true,
        }
    }

    #[test]
    fn mysql_binds_without_a_cast() {
        assert_eq!(MYSQL.bind(1, "int"), "?");
        assert_eq!(MYSQL.placeholder(3), "?");
        assert_eq!(MYSQL.cast_text("id"), "CAST(id AS CHAR)");
        assert_eq!(MYSQL.insert_no_columns("t"), "INSERT INTO t () VALUES ()");
    }

    #[test]
    fn names_identify_the_dialect() {
        assert_eq!(MYSQL.name(), "mysql");
        assert_eq!(MARIADB.name(), "mariadb");
    }

    #[test]
    fn only_mariadb_has_returning_and_sequences() {
        let id = col("id", "char", Some(36));
        assert_eq!(MYSQL.returning(&[&id]), None);
        assert_eq!(
            MARIADB.returning(&[&id]),
            Some("RETURNING CAST(id AS CHAR)".to_string())
        );
        assert_eq!(MYSQL.next_sequence("s"), None);
        assert_eq!(
            MARIADB.next_sequence("s"),
            Some(("SELECT CAST(NEXTVAL(s) AS CHAR)".to_string(), Vec::new()))
        );
    }

    #[test]
    fn returning_is_none_for_an_empty_pk_even_with_the_flag_on() {
        assert_eq!(MARIADB.returning(&[]), None);
    }

    #[test]
    fn returning_joins_a_composite_pk_on_mariadb() {
        let a = col("a", "int", None);
        let b = col("b", "int", None);
        assert_eq!(
            MARIADB.returning(&[&a, &b]),
            Some("RETURNING CAST(a AS CHAR), CAST(b AS CHAR)".to_string())
        );
    }

    #[test]
    fn mariadb_names_the_sequence_inline_because_it_cannot_bind_one() {
        let (sql, binds) = MARIADB.next_sequence("apibdd_it.thing_seq").unwrap();
        assert_eq!(sql, "SELECT CAST(NEXTVAL(apibdd_it.thing_seq) AS CHAR)");
        assert!(binds.is_empty(), "an identifier cannot be a bind parameter");
    }

    #[test]
    fn a_text_uuid_pk_is_generated_but_only_where_it_is_text() {
        assert!(MYSQL.wants_client_uuid(&col("id", "char", Some(36))));
        assert!(MYSQL.wants_client_uuid(&col("id", "varchar", Some(36))));
        assert!(!MYSQL.wants_client_uuid(&col("id", "char", Some(12))));
    }

    #[test]
    fn native_uuid_type_only_counts_on_mariadb() {
        let native = col("id", "uuid", None);
        assert!(MARIADB.wants_client_uuid(&native));
        assert!(!MYSQL.wants_client_uuid(&native));
    }

    #[test]
    fn a_binary_column_is_refused_at_any_length_with_the_reason_in_the_message() {
        let binary16 = col("id", "binary", Some(16));
        let err = MYSQL.check_bindable(&binary16).unwrap_err();
        assert!(err.contains("swap_flag"), "{err}");
        assert!(err.contains("char(36)"), "{err}"); // actionable: what to do instead

        let varbinary16 = col("id", "varbinary", Some(16));
        assert!(MARIADB.check_bindable(&varbinary16).is_err());

        // Not just length 16 — the WHERE-matches-nothing hazard exists at
        // every length: a SHA-1 in binary(20), a packed address in
        // varbinary(16)-turned-8, an HMAC in binary(32).
        let sha1 = col("digest", "binary", Some(20));
        let err = MYSQL.check_bindable(&sha1).unwrap_err();
        assert!(err.contains("digest"), "{err}");
        assert!(!err.contains("swap_flag"), "{err}"); // no UUID story at this length

        // Every binary/varbinary length is refused, not just 16 — item 4
        // widens the rule after the reviewer found the comparison path
        // silently wrong at every length. Only the blob family, and
        // everything non-binary, is left alone.
        assert!(MYSQL.check_bindable(&col("id", "binary", Some(8))).is_err());
        assert!(
            MYSQL
                .check_bindable(&col("name", "varchar", Some(255)))
                .is_ok()
        );
    }

    #[test]
    fn search_path_is_rejected_rather_than_ignored() {
        assert!(MYSQL.session_setup(&[]).unwrap().is_empty());
        assert!(MARIADB.session_setup(&[]).unwrap().is_empty());

        let mysql_err = MYSQL.session_setup(&["apibdd_it".into()]).unwrap_err();
        assert!(mysql_err.contains("schema.table"), "{mysql_err}");
        assert!(mysql_err.starts_with("mysql:"), "{mysql_err}");

        let mariadb_err = MARIADB.session_setup(&["apibdd_it".into()]).unwrap_err();
        assert!(mariadb_err.starts_with("mariadb:"), "{mariadb_err}");
        assert_ne!(mysql_err, mariadb_err, "each engine names itself");
    }

    #[test]
    fn mysql_timestamplike_names_are_its_own_not_postgres() {
        assert!(MYSQL.is_timestamplike("timestamp"));
        assert!(MYSQL.is_timestamplike("datetime"));
        assert!(MYSQL.is_timestamplike("date"));
        assert!(!MYSQL.is_timestamplike("timestamptz"));
    }

    #[test]
    fn introspect_binds_schema_and_table_separately() {
        let tref = TableRef::parse("audit.log").unwrap();
        let (sql, binds) = MYSQL.introspect(&tref);
        assert!(sql.contains("TABLE_SCHEMA"), "{sql}");
        assert_eq!(
            binds,
            vec![Some("audit".to_string()), Some("log".to_string())]
        );

        let bare = TableRef::parse("log").unwrap();
        let (_, binds) = MYSQL.introspect(&bare);
        assert_eq!(binds, vec![None, Some("log".to_string())]);
    }

    #[test]
    fn introspect_sql_produces_every_alias_the_trait_requires() {
        // The trait's own doc says a wrong alias is only found at runtime
        // (`db/introspect.rs` reads by name) — this is the cheapest guard
        // against misspelling one.
        for alias in [
            "AS name",
            "AS type_name",
            "AS length",
            "AS not_null",
            "AS has_default",
            "AS is_identity",
            "AS is_pk",
        ] {
            assert!(
                INTROSPECT_SQL.contains(alias),
                "missing {alias:?} in:\n{INTROSPECT_SQL}"
            );
        }
    }
}
