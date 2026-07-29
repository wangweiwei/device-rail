use std::{path::PathBuf, process::Command};

use serde_json::Value;
use tempfile::TempDir;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../session-bundle/tests/fixtures/protected-omission")
}

#[test]
fn cli_exports_and_validates_without_echoing_local_paths() {
    let temporary = TempDir::new().expect("temporary");
    let output = temporary.path().join("report");
    let binary = env!("CARGO_BIN_EXE_devicerail-report");

    let exported = Command::new(binary)
        .args(["export", "--bundle"])
        .arg(fixture())
        .arg("--output")
        .arg(&output)
        .output()
        .expect("run export");
    assert!(exported.status.success());
    let summary: Value = serde_json::from_slice(&exported.stdout).expect("export summary");
    assert_eq!(summary["ok"], true);
    assert_eq!(summary["operation"], "export");
    assert!(!String::from_utf8_lossy(&exported.stderr).contains(output.to_string_lossy().as_ref()));

    let validated = Command::new(binary)
        .arg("validate")
        .arg(&output)
        .output()
        .expect("run validate");
    assert!(validated.status.success());
    let summary: Value = serde_json::from_slice(&validated.stdout).expect("validate summary");
    assert_eq!(summary["ok"], true);
    assert_eq!(summary["operation"], "validate");

    let usage = Command::new(binary).arg("export").output().expect("usage");
    assert_eq!(usage.status.code(), Some(2));
}
