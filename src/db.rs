//! Pools: one per connection, opened together for the whole run.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use sqlx::any::{AnyPoolOptions, AnyRow};
use sqlx::{AnyPool, ColumnIndex, Row};

use crate::config::Connection;
use crate::engine::{Engine, Family, install_drivers};
use crate::error::StepError;
use crate::sql::introspect::introspect;
use crate::sql::plan::TableSchema;
use crate::sql::reference::TableRef;

/// The connection that could not be opened, and why.
#[derive(Debug)]
pub struct ConnectError {
    pub connection: String,
    pub message: String,
}

struct Pooled {
    pool: AnyPool,
    engine: Engine,
}

/// Every connection of the run, and the schemas read from them so far.
pub struct Db {
    connections: HashMap<String, Pooled>,
    /// Filled on first use and kept for the run, so DDL run mid-suite stays
    /// invisible. Never held across an await.
    schemas: Mutex<HashMap<(String, String), Arc<TableSchema>>>,
}

/// Reads a column as text, tolerating a driver that reports it as a blob
/// instead of text, proven live: `SELECT VERSION()` and
/// `information_schema.DATA_TYPE` come back as blobs on MySQL 8, and
/// `CAST(x AS CHAR)` over a plain `TEXT` column does on MariaDB.
/// `sqlx::Any`'s `String` decode rejects a blob outright. Shared with
/// introspection.
pub fn text_col<I>(row: &AnyRow, index: I) -> Result<Option<String>, String>
where
    I: ColumnIndex<AnyRow> + Copy,
{
    if let Ok(v) = row.try_get::<Option<String>, I>(index) {
        return Ok(v);
    }
    let bytes: Option<Vec<u8>> = row
        .try_get(index)
        .map_err(|e| format!("reading {index:?}: {e}"))?;
    bytes
        .map(String::from_utf8)
        .transpose()
        .map_err(|e| format!("reading {index:?}: not valid UTF-8: {e}"))
}

/// Opens one pool, with the session setup applied to every pooled connection
/// and not only the first, and tells MySQL from MariaDB where the scheme
/// cannot.
async fn open(connection: &Connection, max: u32) -> Result<(AnyPool, Engine), String> {
    install_drivers();
    let family = Family::of(&connection.dsn)?;
    let statements = family.session_setup(&connection.search_path)?;
    let options = AnyPoolOptions::new().max_connections(max.max(1));
    let pool = options
        .after_connect(move |conn, _meta| {
            let statements = statements.clone();
            Box::pin(async move {
                for statement in &statements {
                    sqlx::query(statement).execute(&mut *conn).await?;
                }
                Ok(())
            })
        })
        .connect(&connection.dsn)
        .await
        .map_err(|e| e.to_string())?;

    let engine = match detect(&pool, family).await {
        Ok(engine) => engine,
        Err(error) => {
            pool.close().await;
            return Err(error);
        }
    };
    Ok((pool, engine))
}

/// Postgres is settled by its scheme; MySQL and MariaDB share one, so only
/// there does it take a query.
async fn detect(pool: &AnyPool, family: Family) -> Result<Engine, String> {
    match family {
        Family::Postgres => Ok(Engine::Postgres),
        Family::Mysql => {
            let detecting = |e: String| format!("detecting the vendor: {e}");
            let row = sqlx::query("SELECT VERSION()")
                .fetch_one(pool)
                .await
                .map_err(|e| detecting(e.to_string()))?;
            let version = text_col(&row, 0).map_err(detecting)?.unwrap_or_default();
            Ok(Engine::from_version(&version))
        }
    }
}

/// What `doctor --live` does: connect that one connection, report the engine,
/// and let go.
pub async fn probe(connection: &Connection) -> Result<Engine, String> {
    let (pool, engine) = open(connection, 1).await?;
    pool.close().await;
    Ok(engine)
}

impl Db {
    /// Opens the connections in name order and stops at the first that fails.
    pub async fn connect(
        connections: &BTreeMap<String, Connection>,
        max: u32,
    ) -> Result<Self, ConnectError> {
        let mut opened = HashMap::new();
        for (name, connection) in connections {
            match open(connection, max).await {
                Ok((pool, engine)) => {
                    opened.insert(name.clone(), Pooled { pool, engine });
                }
                Err(message) => {
                    // Release what is already open now, not whenever sqlx
                    // gets to the drop.
                    Self::new(opened).close().await;
                    return Err(ConnectError {
                        connection: name.clone(),
                        message,
                    });
                }
            }
        }
        Ok(Self::new(opened))
    }

    fn new(connections: HashMap<String, Pooled>) -> Self {
        Self {
            connections,
            schemas: Mutex::default(),
        }
    }

    /// The one place that knows "not declared" for a connection name.
    fn connection(&self, name: &str) -> Result<&Pooled, StepError> {
        self.connections.get(name).ok_or_else(|| {
            StepError::input(format!(
                "connection {name:?} is not declared in resources.db"
            ))
        })
    }

    /// One table's columns, read from the database the first time and then
    /// from the cache.
    pub async fn schema(
        &self,
        connection: &str,
        table: &TableRef,
    ) -> Result<Arc<TableSchema>, StepError> {
        let key = (connection.to_string(), table.sql_name());
        if let Some(schema) = self.cached(&key) {
            return Ok(schema);
        }
        let pooled = self.connection(connection)?;
        let schema = Arc::new(introspect(&pooled.pool, pooled.engine.platform(), table).await?);
        self.schemas
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, Arc::clone(&schema));
        Ok(schema)
    }

    fn cached(&self, key: &(String, String)) -> Option<Arc<TableSchema>> {
        self.schemas
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .map(Arc::clone)
    }

    pub fn engine(&self, connection: &str) -> Option<Engine> {
        self.connections.get(connection).map(|c| c.engine)
    }

    pub async fn close(&self) {
        for connection in self.connections.values() {
            connection.pool.close().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unknown_scheme_is_refused_by_name() {
        let connections = BTreeMap::from([(
            "primary".to_string(),
            Connection {
                dsn: "mssql://x/y".into(),
                search_path: Vec::new(),
            },
        )]);
        let Err(error) = Db::connect(&connections, 1).await else {
            panic!("connecting to an unknown scheme should fail");
        };
        // "primary" pins that the name survives, and "mssql" that the scheme
        // is rejected by name, not dialed as a hostname and failed on DNS.
        assert_eq!(error.connection, "primary");
        assert!(error.message.contains("mssql"), "{}", error.message);
    }

    #[tokio::test]
    async fn a_schema_of_an_undeclared_connection_is_an_input_error() {
        let db = Db::new(HashMap::new());
        let tref = TableRef::parse("t").expect("ref");
        let Err(error) = db.schema("x", &tref).await else {
            panic!("an undeclared connection must be refused");
        };
        assert_eq!(error.class, crate::error::Class::Input);
        assert!(error.message.contains(r#""x""#), "{}", error.message);
    }
}
