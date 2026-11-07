/// A table reference of the form `[connection:][schema.]table`.
/// The separators do not overlap: `:` separates the connection, `.` the schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRef {
    pub conn: Option<String>,
    pub schema: Option<String>,
    pub table: String,
}

impl TableRef {
    pub fn parse(s: &str) -> Result<TableRef, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty table reference".into());
        }
        let (conn, rest) = match s.split_once(':') {
            Some((c, r)) => (Some(c.trim().to_string()), r.trim()),
            None => (None, s),
        };
        let (schema, table) = match rest.rsplit_once('.') {
            Some((sch, t)) => (Some(sch.trim().to_string()), t.trim().to_string()),
            None => (None, rest.to_string()),
        };
        if table.is_empty() {
            return Err(format!("reference {s:?} does not name a table"));
        }
        Ok(TableRef {
            conn,
            schema,
            table,
        })
    }

    /// Name for SQL and for `to_regclass`. Not quoted: names are plain lowercase.
    pub fn sql_name(&self) -> String {
        match &self.schema {
            Some(sch) => format!("{sch}.{}", self.table),
            None => self.table.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_table() {
        let r = TableRef::parse("users").unwrap();
        assert_eq!(
            r,
            TableRef {
                conn: None,
                schema: None,
                table: "users".into()
            }
        );
        assert_eq!(r.sql_name(), "users");
    }

    #[test]
    fn schema_qualified() {
        let r = TableRef::parse("audit.log").unwrap();
        assert_eq!(r.schema.as_deref(), Some("audit"));
        assert_eq!(r.table, "log");
        assert_eq!(r.sql_name(), "audit.log");
    }

    #[test]
    fn connection_and_schema() {
        let r = TableRef::parse("billing:public.invoices").unwrap();
        assert_eq!(r.conn.as_deref(), Some("billing"));
        assert_eq!(r.schema.as_deref(), Some("public"));
        assert_eq!(r.table, "invoices");
        assert_eq!(r.sql_name(), "public.invoices");
    }

    #[test]
    fn connection_without_schema() {
        let r = TableRef::parse("billing:invoices").unwrap();
        assert_eq!(r.conn.as_deref(), Some("billing"));
        assert_eq!(r.schema, None);
        assert_eq!(r.table, "invoices");
    }

    #[test]
    fn empty_is_error() {
        assert!(TableRef::parse("   ").is_err());
    }

    #[test]
    fn missing_table_after_schema_is_error() {
        assert!(TableRef::parse("audit.").is_err());
    }
}
