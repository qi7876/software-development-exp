#![allow(clippy::expect_used)]

use data_backup_protocol::{ComponentStatus, PROTOCOL_VERSION};

#[test]
fn component_status_round_trips_through_json() {
    let status = ComponentStatus::ok("data-backup", "0.1.0");
    let json = serde_json::to_string(&status).expect("status must serialize");
    let decoded: ComponentStatus =
        serde_json::from_str(&json).expect("serialized status must deserialize");

    assert_eq!(decoded, status);
    assert_eq!(decoded.protocol_version, PROTOCOL_VERSION);
    assert_eq!(decoded.status, "ok");
}
