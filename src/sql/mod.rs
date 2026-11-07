//! The SQL layer: table references, value parsing, statement building, the
//! three engines' dialects and schema introspection. No step runs here; the
//! steps use it.

pub mod introspect;
pub mod plan;
pub mod platform;
pub mod reference;
pub mod value;

use sqlx::Any;
use sqlx::any::AnyArguments;
use sqlx::query::Query;

use crate::error::{Class, StepError};

/// Binds text parameters (`Option<String>`). Every value crosses the wire as
/// text; the SQL string itself carries the type, through whatever
/// `Platform::bind` produced: a `$N::type` cast on Postgres, a bare `?` on
/// MySQL and MariaDB, which coerce implicitly. Shared by step execution and
/// introspection.
pub fn bind_all<'q>(
    mut q: Query<'q, Any, AnyArguments<'q>>,
    binds: &'q [Option<String>],
) -> Query<'q, Any, AnyArguments<'q>> {
    for b in binds {
        q = q.bind(b);
    }
    q
}

/// The class of a driver error, decided by what the driver says it is. The
/// error type is `non_exhaustive`, so the arm that names what is left is the
/// statement: whatever else goes wrong came back from a connection that worked.
pub fn sql_kind(error: &sqlx::Error) -> Class {
    match error {
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => Class::DbConnection,
        // The server itself can say the connection is gone: SQLSTATE class 08
        // (connection exception) and Postgres's 57P01..57P03 (the server is
        // shutting down or cannot accept the connection).
        sqlx::Error::Database(e)
            if e.code().is_some_and(|c| {
                c.starts_with("08") || ["57P01", "57P02", "57P03"].contains(&&*c)
            }) =>
        {
            Class::DbConnection
        }
        _ => Class::DbStatement,
    }
}

/// A failed statement, classified from the driver error and worded by the caller.
pub fn sql_failure(error: &sqlx::Error, message: String) -> StepError {
    StepError::new(sql_kind(error), message)
}

/// A result the database sent that could not be read: the statement ran, the
/// answer is not what the step expects.
pub fn db_read(message: impl Into<String>) -> StepError {
    StepError::new(Class::DbStatement, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::error::Error;
    use std::fmt;

    /// A server error carrying only a SQLSTATE.
    #[derive(Debug)]
    struct Fake(Option<&'static str>);

    impl fmt::Display for Fake {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("fake")
        }
    }

    impl Error for Fake {}

    impl sqlx::error::DatabaseError for Fake {
        fn message(&self) -> &'static str {
            "fake"
        }
        fn code(&self) -> Option<Cow<'_, str>> {
            self.0.map(Cow::Borrowed)
        }
        fn as_error(&self) -> &(dyn Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    fn server(code: Option<&'static str>) -> sqlx::Error {
        sqlx::Error::Database(Box::new(Fake(code)))
    }

    #[test]
    fn a_lost_connection_is_a_db_connection_failure_and_a_refusal_is_a_statement() {
        let lost = sqlx::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionReset));
        assert_eq!(sql_kind(&lost), Class::DbConnection);
        assert_eq!(sql_kind(&sqlx::Error::PoolTimedOut), Class::DbConnection);
        assert_eq!(sql_kind(&sqlx::Error::RowNotFound), Class::DbStatement);
    }

    #[test]
    fn every_connection_level_driver_error_is_a_db_connection_failure() {
        let tls = sqlx::Error::Tls("handshake".into());
        for error in [tls, sqlx::Error::PoolClosed, sqlx::Error::WorkerCrashed] {
            assert_eq!(sql_kind(&error), Class::DbConnection, "{error:?}");
        }
    }

    #[test]
    fn a_server_that_says_the_connection_is_gone_is_a_db_connection_failure() {
        for code in ["08000", "08006", "57P01", "57P02", "57P03"] {
            assert_eq!(sql_kind(&server(Some(code))), Class::DbConnection, "{code}");
        }
    }

    #[test]
    fn any_other_server_error_is_a_statement_failure() {
        for code in [
            Some("57P04"),
            Some("42P01"),
            Some("23505"),
            Some("0A000"),
            None,
        ] {
            assert_eq!(sql_kind(&server(code)), Class::DbStatement, "{code:?}");
        }
    }

    #[test]
    fn a_failure_keeps_the_class_of_the_driver_error() {
        let e = sql_failure(&sqlx::Error::PoolClosed, "boom".into());
        assert_eq!((e.class, e.message.as_str()), (Class::DbConnection, "boom"));
        assert_eq!(db_read("odd").class, Class::DbStatement);
    }
}
