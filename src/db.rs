//! Pools: one per connection, opened together for the whole run.

use std::collections::{BTreeMap, HashMap};

use sqlx::any::{AnyPoolOptions, AnyRow};
use sqlx::{AnyPool, ColumnIndex, Row};

use crate::config::Connection;
use crate::engine::{Engine, Family, install_drivers};

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

/// Every connection of the run. Holds nothing that changes after `connect`.
pub struct Db {
    connections: HashMap<String, Pooled>,
}

/// Reads a column as text, tolerating a driver that reports it as a blob
/// instead of text: `SELECT VERSION()` comes back as a blob on some MySQL
/// builds, and `sqlx::Any`'s `String` decode rejects a blob outright.
fn text_col<I>(row: &AnyRow, index: I) -> Result<Option<String>, String>
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
                    Self {
                        connections: opened,
                    }
                    .close()
                    .await;
                    return Err(ConnectError {
                        connection: name.clone(),
                        message,
                    });
                }
            }
        }
        Ok(Self {
            connections: opened,
        })
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
