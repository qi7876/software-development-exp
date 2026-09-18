#![allow(clippy::expect_used)]

use data_backup_core::{RepositoryId, TaskId, validate_protocol_version};
use data_backup_protocol::PROTOCOL_VERSION;

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

#[test]
fn protocol_compatibility_is_explicit() {
    assert!(validate_protocol_version(PROTOCOL_VERSION).is_ok());
    assert!(validate_protocol_version(PROTOCOL_VERSION + 1).is_err());
}
