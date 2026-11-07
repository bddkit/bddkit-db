//! One `resources.db.<name>` body: what the host hands over verbatim, minus
//! its reserved `options` key. Unknown keys are ignored.

use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
pub struct Connection {
    pub dsn: String,
    #[serde(default)]
    pub search_path: Vec<String>,
}

impl Connection {
    pub fn parse(config: &Value) -> Result<Self, String> {
        Self::deserialize(config).map_err(|e| e.to_string())
    }
}

/// What `bddkit resource fields` and `resource add` know about the keys.
/// Kept beside `Connection` so a key added to one is not missed in the other.
pub fn fields_json() -> Value {
    json!([
        {
            "name": "dsn",
            "required": true,
            "description": "connection string; its scheme selects the engine (postgres://, mysql://)"
        },
        {
            "name": "search_path",
            "type": "nonscalar",
            "description": "Postgres schema search path; refused on MySQL and MariaDB, which have no session equivalent"
        },
        {
            "name": "options",
            "type": "nonscalar",
            "description": "polling.timeout_secs / polling.interval_ms for eventual assertions against this connection"
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_keys_are_ignored_and_search_path_defaults_to_empty() {
        let c = Connection::parse(&json!({"dsn": "postgres://x", "colour": "red"})).expect("ok");
        assert_eq!(c.dsn, "postgres://x");
        assert!(c.search_path.is_empty());
    }

    #[test]
    fn a_search_path_that_is_not_a_list_is_refused() {
        let error = Connection::parse(&json!({"dsn": "postgres://x", "search_path": "a"}))
            .expect_err("a string is not a list");
        assert!(error.contains("expected a sequence"), "{error}");
    }

    #[test]
    fn a_missing_dsn_is_refused() {
        assert!(Connection::parse(&json!({})).is_err());
    }

    #[test]
    fn every_field_the_connection_reads_is_described() {
        let names: Vec<_> = fields_json()
            .as_array()
            .expect("array")
            .iter()
            .map(|f| f["name"].as_str().expect("name").to_string())
            .collect();
        assert_eq!(names, ["dsn", "search_path", "options"]);
    }
}
