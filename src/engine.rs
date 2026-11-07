//! Which engine a DSN names, decided without opening a socket.

use std::str::FromStr;
use std::sync::Once;

use sqlx::any::AnyConnectOptions;

use crate::config::Connection;
use crate::sql::platform::{MARIADB, MYSQL, PG, Platform};

/// The family a DSN scheme selects. MySQL and MariaDB share the `mysql://`
/// scheme, so telling them apart takes a query: see [`Engine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Postgres,
    Mysql,
}

/// What a live connection turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Postgres,
    MySql,
    MariaDb,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
            Self::MariaDb => "mariadb",
        }
    }

    /// The dialect this engine speaks.
    pub fn platform(self) -> &'static dyn Platform {
        match self {
            Self::Postgres => &PG,
            Self::MySql => &MYSQL,
            Self::MariaDb => &MARIADB,
        }
    }

    /// The vendor a `mysql://` server's `SELECT VERSION()` names.
    pub fn from_version(version: &str) -> Self {
        if version.contains("MariaDB") {
            Self::MariaDb
        } else {
            Self::MySql
        }
    }
}

impl Family {
    pub fn of(dsn: &str) -> Result<Self, String> {
        // A DSN without `://` may be `user:secret@host/db`, so it is never
        // quoted: validation errors are printed without the secret filter.
        let Some((scheme, _)) = dsn.split_once("://") else {
            return Err(
                "the DSN has no scheme (expected postgres, postgresql, mysql or mariadb)".into(),
            );
        };
        match scheme {
            "postgres" | "postgresql" => Ok(Self::Postgres),
            // sqlx's MySQL driver answers to both schemes, so refusing
            // "mariadb" here would reject a DSN the driver would connect.
            "mysql" | "mariadb" => Ok(Self::Mysql),
            other => Err(format!(
                "unknown database scheme {other:?} (expected postgres, postgresql, mysql or mariadb)"
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Postgres => Engine::Postgres.name(),
            Self::Mysql => Engine::MySql.name(),
        }
    }

    /// Statements to run right after every pooled connection opens. The two
    /// `mysql://` vendors share one answer, so the family's platform decides.
    pub fn session_setup(self, search_path: &[String]) -> Result<Vec<String>, String> {
        match self {
            Self::Postgres => PG.session_setup(search_path),
            Self::Mysql => MYSQL.session_setup(search_path),
        }
    }
}

/// The sqlx `Any` driver must be registered before it parses or connects.
pub fn install_drivers() {
    static ONCE: Once = Once::new();
    ONCE.call_once(sqlx::any::install_default_drivers);
}

/// Everything a connection can be refused for without opening a socket: the
/// scheme, a `search_path` its engine cannot honour, and a DSN sqlx cannot
/// parse. The scheme is only the first word of the DSN, and the pool parses
/// the whole URL before it connects, so checking less here would certify a
/// config the run refuses.
pub fn check(connection: &Connection) -> Result<Family, String> {
    install_drivers();
    let family = Family::of(&connection.dsn)?;
    family.session_setup(&connection.search_path)?;
    AnyConnectOptions::from_str(&connection.dsn).map_err(|e| e.to_string())?;
    Ok(family)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(dsn: &str, search_path: &[&str]) -> Connection {
        Connection {
            dsn: dsn.into(),
            search_path: search_path.iter().map(|s| (*s).into()).collect(),
        }
    }

    #[test]
    fn the_scheme_selects_the_family() {
        for (dsn, family) in [
            ("postgres://h/d", Family::Postgres),
            ("postgresql://h/d", Family::Postgres),
            ("mysql://h/d", Family::Mysql),
            ("mariadb://h/d", Family::Mysql),
        ] {
            assert_eq!(Family::of(dsn), Ok(family), "{dsn}");
        }
        let unknown = Family::of("sqlite://x").expect_err("unknown scheme");
        assert!(unknown.contains("sqlite"), "{unknown}");
        let leak = Family::of("user:secret@host/db").expect_err("no scheme");
        assert!(!leak.contains("secret"), "{leak}");
    }

    #[test]
    fn postgres_sets_the_search_path_and_mysql_refuses_one() {
        assert_eq!(Family::Postgres.session_setup(&[]), Ok(vec![]));
        assert_eq!(
            Family::Postgres.session_setup(&["a".into(), "b".into()]),
            Ok(vec!["SET search_path TO a, b".to_string()])
        );
        assert_eq!(Family::Mysql.session_setup(&[]), Ok(vec![]));
        assert!(Family::Mysql.session_setup(&["a".into()]).is_err());
    }

    #[test]
    fn a_dsn_is_parsed_whole_not_only_by_its_scheme() {
        // Port 1 is closed on any sane host: `check` must not connect.
        assert_eq!(
            check(&connection("postgres://u:p@127.0.0.1:1/x", &["app"])),
            Ok(Family::Postgres)
        );
        let port = check(&connection("postgres://u:p@127.0.0.1:notaport/x", &[]))
            .expect_err("malformed port");
        assert!(port.contains("port"), "{port}");
        let path = check(&connection("mysql://u:p@127.0.0.1:3306/x", &["s"]))
            .expect_err("search_path on mysql");
        assert!(path.contains("search_path"), "{path}");
    }

    #[test]
    fn each_engine_speaks_its_own_dialect() {
        for engine in [Engine::Postgres, Engine::MySql, Engine::MariaDb] {
            assert_eq!(engine.platform().name(), engine.name());
        }
    }

    #[test]
    fn the_version_string_tells_mariadb_from_mysql() {
        assert_eq!(Engine::from_version("11.4.2-MariaDB-ubu"), Engine::MariaDb);
        assert_eq!(Engine::from_version("8.0.39"), Engine::MySql);
    }
}
