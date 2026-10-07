//! Domain identifiers retained inside the single server package.

use thiserror::Error;

/// A domain identifier is empty or only contains whitespace.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("{kind} must not be empty")]
pub struct EmptyIdentifier {
    /// Human-readable identifier kind.
    pub kind: &'static str,
}

macro_rules! identifier {
    ($name:ident, $kind:literal) => {
        #[doc = concat!("Validated ", $kind, ".")]
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Parse a non-empty ", $kind, ".")]
            pub fn parse(value: impl Into<String>) -> Result<Self, EmptyIdentifier> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(EmptyIdentifier { kind: $kind });
                }
                Ok(Self(value))
            }

            /// Borrow the validated identifier value.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier!(TaskId, "task identifier");
identifier!(RepositoryId, "repository identifier");

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn identifiers_reject_blank_values() {
        assert!(TaskId::parse("  ").is_err());
        assert!(RepositoryId::parse("").is_err());
    }

    #[test]
    fn identifiers_preserve_valid_values() {
        let task_id = TaskId::parse("photos").expect("non-empty task id must be valid");
        assert_eq!(task_id.as_str(), "photos");
    }
}
