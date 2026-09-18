//! Transport-independent types shared by Data Backup processes.

use serde::{Deserialize, Serialize};

/// Version of the local application protocol understood by this workspace.
pub const PROTOCOL_VERSION: u16 = 1;

/// Machine-readable result returned by framework self-check commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentStatus {
    /// Executable or library reporting its status.
    pub component: String,
    /// Cargo package version of the reporting component.
    pub package_version: String,
    /// Local protocol version supported by the component.
    pub protocol_version: u16,
    /// Stable status label. Framework checks currently emit `ok`.
    pub status: String,
}

impl ComponentStatus {
    /// Create a successful framework compatibility result.
    #[must_use]
    pub fn ok(component: impl Into<String>, package_version: impl Into<String>) -> Self {
        Self {
            component: component.into(),
            package_version: package_version.into(),
            protocol_version: PROTOCOL_VERSION,
            status: "ok".to_owned(),
        }
    }
}
