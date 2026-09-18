//! Errors exposed by the stable core contracts.

use thiserror::Error;

/// Failures detected before any backup I/O starts.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    /// A domain identifier is empty or only contains whitespace.
    #[error("{kind} must not be empty")]
    EmptyIdentifier {
        /// Human-readable identifier kind.
        kind: &'static str,
    },
    /// A caller and the core disagree on the local protocol version.
    #[error("unsupported protocol version {actual}; expected {expected}")]
    UnsupportedProtocolVersion {
        /// Protocol version supported by the core.
        expected: u16,
        /// Protocol version supplied by the caller.
        actual: u16,
    },
}
