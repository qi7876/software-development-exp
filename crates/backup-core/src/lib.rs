//! Stable domain contracts and module boundaries for the backup core.

pub mod application;
pub mod domain;
mod error;
mod identifiers;
pub mod pipeline;
pub mod ports;

pub use error::CoreError;
pub use identifiers::{RepositoryId, TaskId};

use data_backup_protocol::{ComponentStatus, PROTOCOL_VERSION};

/// Validate that a caller uses the protocol version supported by this core.
pub fn validate_protocol_version(actual: u16) -> Result<(), CoreError> {
    if actual == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(CoreError::UnsupportedProtocolVersion {
            expected: PROTOCOL_VERSION,
            actual,
        })
    }
}

/// Run the non-I/O framework compatibility check used by the binaries.
pub fn framework_status(
    component: &str,
    package_version: &str,
) -> Result<ComponentStatus, CoreError> {
    validate_protocol_version(PROTOCOL_VERSION)?;
    Ok(ComponentStatus::ok(component, package_version))
}
