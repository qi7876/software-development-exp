//! Validated identifiers used by the domain model.

use crate::CoreError;

macro_rules! identifier {
    ($name:ident, $kind:literal) => {
        #[doc = concat!("Validated ", $kind, ".")]
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            #[doc = concat!("Parse a non-empty ", $kind, ".")]
            pub fn parse(value: impl Into<String>) -> Result<Self, CoreError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(CoreError::EmptyIdentifier { kind: $kind });
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
