//! `void version --check` and `void self-update` against fake signed
//! releases in a sandbox: no network, and the binary replaced is a stand-in.

#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use deepclean_core::testkit::release::fake_release;
use serde_json::Value;

fn void(home: &Path, release: &Path, target: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_void"))
        .arg("--home")
        .arg(home)
        .args(args)
        .env("VOID_UPDATE_FEED", release.join("releases.json"))
        .env("VOID_UPDATE_KEY", release.join("release-key.pem"))
        .env("VOID_UPDATE_TARGET", target)
        .output()
        .unwrap()
}

fn stand_in(path: &Path, version: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\necho void {version}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn version_check_reports_current_and_latest_as_json() {
    let tmp = tempfile::tempdir().unwrap();
    let r = tmp.path().join("rel");
    fake_release(&r, "9.0.0", "1.0.0", false).unwrap();
    let out = void(
        tmp.path(),
        &r,
        &tmp.path().join("void"),
        &["--json", "version", "--check"],
    );
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["current"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["latest"], "9.0.0");
    assert_eq!(v["update_available"], true);
}

#[test]
fn a_sandbox_without_a_local_feed_never_checks() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_void"))
        .arg("--home")
        .arg(tmp.path())
        .args(["version", "--check"])
        .env_remove("VOID_UPDATE_FEED")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("sandbox"));
}

#[test]
fn self_update_refuses_a_tampered_binary_and_installs_a_verified_one() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("void");
    stand_in(&target, "1.0.0");

    let bad = tmp.path().join("bad");
    fake_release(&bad, "9.0.0", "1.0.0", true).unwrap();
    let out = void(tmp.path(), &bad, &target, &["self-update"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("checksum"));
    assert!(std::fs::read_to_string(&target).unwrap().contains("1.0.0"));
    assert!(!tmp.path().join(".void-update").exists());

    let good = tmp.path().join("good");
    fake_release(&good, "9.0.0", "1.0.0", false).unwrap();
    let out = void(tmp.path(), &good, &target, &["--json", "self-update"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["updated"], true);
    assert_eq!(v["to"], "9.0.0");
    let ran = Command::new(&target).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&ran.stdout).trim(), "void 9.0.0");
}
