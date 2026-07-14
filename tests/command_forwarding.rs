#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn prefix_help_does_not_execute_non_cargo_plugin() {
    let temp = tempdir().unwrap();
    let plugin_dir = temp.path().join("bin");
    fs::create_dir(&plugin_dir).unwrap();
    let marker = temp.path().join("plugin-executed");

    write_executable(
        &plugin_dir.join("meta-git"),
        r#"#!/bin/sh
if [ "$1" = "--meta-plugin-info" ]; then
  printf '%s\n' '{"name":"git","version":"1.0.0","commands":["git"]}'
  exit 0
fi
if [ "$1" = "--meta-plugin-exec" ]; then
  IFS= read -r request || :
  : > "$META_TEST_MARKER"
  printf '%s\n' '{"plan":{"commands":[]}}'
  exit 0
fi
exit 1
"#,
    );

    let run = |args: &[&str]| {
        Command::new(assert_cmd::cargo::cargo_bin!("meta"))
            .current_dir(temp.path())
            .env("PATH", &plugin_dir)
            .env("HOME", temp.path())
            .env("META_DATA_DIR", temp.path().join("meta-data"))
            .env("META_TEST_MARKER", &marker)
            .args(args)
            .output()
            .unwrap()
    };

    for args in [
        &["--help", "git", "pull"][..],
        &["--help", "git", "clone", "https://example.invalid/repo.git"][..],
    ] {
        let output = run(args);
        assert!(output.status.success(), "args: {args:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
        assert!(!marker.exists(), "prefix help executed {args:?}");
    }

    fs::write(temp.path().join(".meta"), r#"{"projects":{}}"#).unwrap();
    let output = run(&["--help", "git", "pull"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
    assert!(!marker.exists(), "prefix help executed in a Meta workspace");
}

#[test]
fn child_only_plugin_plan_keeps_the_actual_meta_root_label() {
    let temp = tempdir().unwrap();
    let plugin_dir = temp.path().join("bin");
    let child = temp.path().join("child");
    fs::create_dir(&plugin_dir).unwrap();
    fs::create_dir(&child).unwrap();
    fs::write(
        temp.path().join(".meta"),
        r#"{"projects":{"child":{"repo":"https://example.invalid/child.git","path":"child"}}}"#,
    )
    .unwrap();

    write_executable(
        &plugin_dir.join("meta-rust"),
        r#"#!/bin/sh
if [ "$1" = "--meta-plugin-info" ]; then
  printf '%s\n' '{"name":"rust","version":"1.0.0","commands":["cargo","rust"]}'
  exit 0
fi
if [ "$1" = "--meta-plugin-exec" ]; then
  IFS= read -r request || :
  printf '{"plan":{"commands":[{"dir":"%s","cmd":"printf child-output"}],"parallel":false}}\n' "$META_TEST_CHILD"
  exit 0
fi
exit 1
"#,
    );

    let data_dir = temp.path().join("meta-data");
    fs::create_dir(&data_dir).unwrap();
    let output = Command::new(assert_cmd::cargo::cargo_bin!("meta"))
        .current_dir(temp.path())
        .env("PATH", &plugin_dir)
        .env("HOME", temp.path())
        .env("META_DATA_DIR", data_dir)
        .env("META_TEST_CHILD", &child)
        .args(["--sequential", "--include", "child", "cargo", "check"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("✓ child"), "stdout: {stdout}");
    assert!(!stdout.contains("✓ . (child)"), "stdout: {stdout}");
}
