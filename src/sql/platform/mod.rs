mod mysql;
mod postgres;

pub use mysql::{MARIADB, MYSQL};
pub use postgres::PG;

use crate::sql::plan::Column;
use crate::sql::reference::TableRef;

/// The engine-specific SQL dialect. Every connection is reached through a
/// `&'static dyn Platform` — nothing is allocated per connection.
pub trait Platform: Send + Sync {
    fn name(&self) -> &'static str;
    fn bind(&self, n: usize, ty: &str) -> String;
    fn placeholder(&self, n: usize) -> String;
    fn cast_text(&self, expr: &str) -> String;
    fn insert_no_columns(&self, table: &str) -> String;
    fn returning(&self, pk: &[&Column]) -> Option<String>;
    fn is_timestamplike(&self, ty: &str) -> bool;
    fn wants_client_uuid(&self, col: &Column) -> bool;
    fn check_bindable(&self, col: &Column) -> Result<(), String>;

    /// The introspection query for one table, plus its binds. `db/introspect.rs`
    /// reads the result set by column name, with no compile-time check that the
    /// query actually produces them — a wrong alias here is an `Err` naming the
    /// column, from `try_get`, there. The query MUST return exactly these seven aliases:
    /// - `name` (text) — column name
    /// - `type_name` (text) — the platform's native type name, as used by `bind`/`returning`
    /// - `length` (int, nullable) — declared length/precision, e.g. `char(36)`;
    ///   NULL where the platform never needs it (Postgres has a real `uuid` type)
    /// - `not_null` (int, 0/1) — `AnyRow`'s `bool` decoding differs per driver
    /// - `has_default` (int, 0/1)
    /// - `is_identity` (int, 0/1)
    /// - `is_pk` (int, 0/1)
    fn introspect(&self, tref: &TableRef) -> (String, Vec<Option<String>>);

    fn next_sequence(&self, seq: &str) -> Option<(String, Vec<Option<String>>)>;

    /// One or more session-setup statements to run right after connecting.
    /// `search_path` is `resources.db.<name>.search_path` from the config,
    /// verbatim. Empty input means no statement is needed.
    fn session_setup(&self, search_path: &[String]) -> Result<Vec<String>, String>;
}
