#![allow(clippy::expect_used)]

use std::process::Command;

#[test]
fn check_reports_compatible_framework() {
    let output = Command::new(env!("CARGO_BIN_EXE_data-backup"))
        .arg("check")
        .output()
        .expect("CLI must start");

    assert!(output.status.success());
    let body: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("check output must be JSON");
    assert_eq!(body["component"], "data-backup");
    assert_eq!(body["status"], "ok");
}

#[test]
fn unknown_command_fails() {
    let status = Command::new(env!("CARGO_BIN_EXE_data-backup"))
        .arg("unknown")
        .status()
        .expect("CLI must start");
    assert!(!status.success());
}
