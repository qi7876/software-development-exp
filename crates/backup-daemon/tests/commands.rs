#![allow(clippy::expect_used)]

use std::process::Command;

#[test]
fn check_reports_compatible_framework() {
    let output = Command::new(env!("CARGO_BIN_EXE_data-backupd"))
        .arg("check")
        .output()
        .expect("daemon executable must start");

    assert!(output.status.success());
    let body: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("check output must be JSON");
    assert_eq!(body["component"], "data-backupd");
    assert_eq!(body["status"], "ok");
}
