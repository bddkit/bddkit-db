//! Why a step failed: its class and the text a person reads. The class is set
//! where the error is born and is never parsed back out of the message, and
//! there is deliberately no `From<String>`: a `?` on a plain string must not
//! compile, because a silent default class is how the scheme would rot.

use std::fmt;

/// The failure classes a database step can end in, named as the host's
/// `class` field spells them. Every `match` on it is exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// A connection could not be opened or was lost.
    DbConnection,
    /// The database refused a statement, or its answer could not be read.
    DbStatement,
    /// A check did not hold.
    Assertion,
    /// The step cannot run as written.
    Input,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DbConnection => "db_connection",
            Self::DbStatement => "db_statement",
            Self::Assertion => "assertion",
            Self::Input => "input",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepError {
    pub class: Class,
    pub message: String,
}

impl StepError {
    pub fn new(class: Class, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
        }
    }

    pub fn input(message: impl Into<String>) -> Self {
        Self::new(Class::Input, message)
    }

    pub fn assertion(message: impl Into<String>) -> Self {
        Self::new(Class::Assertion, message)
    }
}

impl fmt::Display for StepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_class_is_spelled_as_the_host_expects() {
        for (class, name) in [
            (Class::DbConnection, "db_connection"),
            (Class::DbStatement, "db_statement"),
            (Class::Assertion, "assertion"),
            (Class::Input, "input"),
        ] {
            assert_eq!(class.as_str(), name);
        }
    }
}
