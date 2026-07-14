#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn path_with_plugin(plugin_dir: &Path) -> OsString {
    let mut paths = vec![plugin_dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(paths).unwrap()
}

fn meta_command(current_dir: &Path, plugin_dir: &Path, home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_meta"));
    command
        .current_dir(current_dir)
        .env("PATH", path_with_plugin(plugin_dir))
        .env("HOME", home)
        .env("META_DATA_DIR", home.join("meta-data"));
    command
}

fn create_plugin_dir(root: &Path) -> PathBuf {
    let plugin_dir = root.join("bin");
    fs::create_dir(&plugin_dir).unwrap();
    plugin_dir
}

#[test]
fn prefix_help_does_not_execute_plugin_commands() {
    let temp = tempdir().unwrap();
    let plugin_dir = create_plugin_dir(temp.path());
    let marker = temp.path().join("plugin-executed");

    write_executable(
        &plugin_dir.join("meta-tool"),
        r#"#!/bin/sh
if [ "$1" = "--meta-plugin-info" ]; then
  printf '%s\n' '{"name":"tool","version":"1.0.0","commands":["tool"]}'
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

    for args in [
        &["--help", "tool", "run"][..],
        &["--help", "tool", "run", "payload"][..],
    ] {
        let output = meta_command(temp.path(), &plugin_dir, temp.path())
            .env("META_TEST_MARKER", &marker)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "args: {args:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
        assert!(!marker.exists(), "prefix help executed {args:?}");
    }

    fs::write(temp.path().join(".meta"), r#"{"projects":{}}"#).unwrap();
    let output = meta_command(temp.path(), &plugin_dir, temp.path())
        .env("META_TEST_MARKER", &marker)
        .args(["--help", "tool", "run"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!marker.exists(), "prefix help executed in a Meta workspace");
}

#[test]
fn nested_plugin_help_skips_workspace_config_and_reaches_plugin() {
    let temp = tempdir().unwrap();
    let plugin_dir = create_plugin_dir(temp.path());
    let request_path = temp.path().join("plugin-request.json");
    fs::write(temp.path().join(".meta.yaml"), "projects: [malformed").unwrap();

    write_executable(
        &plugin_dir.join("meta-tool"),
        r#"#!/bin/sh
if [ "$1" = "--meta-plugin-info" ]; then
  printf '%s\n' '{"name":"tool","version":"1.0.0","commands":["tool"]}'
  exit 0
fi
if [ "$1" = "--meta-plugin-exec" ]; then
  IFS= read -r request || :
  printf '%s\n' "$request" > "$META_TEST_REQUEST"
  printf '%s\n' 'tool-owned nested help'
  exit 0
fi
exit 1
"#,
    );

    let output = meta_command(temp.path(), &plugin_dir, temp.path())
        .env("META_TEST_REQUEST", &request_path)
        .args(["tool", "run", "--help"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "tool-owned nested help"
    );

    let request: serde_json::Value =
        serde_json::from_slice(&fs::read(&request_path).unwrap()).unwrap();
    assert_eq!(request["command"], "tool");
    assert_eq!(request["args"], serde_json::json!(["run", "--help"]));
    assert_eq!(request["projects"], serde_json::json!([]));
}

#[test]
fn declared_bare_help_is_metadata_only_without_reclassifying_other_commands() {
    let temp = tempdir().unwrap();
    let plugin_dir = create_plugin_dir(temp.path());
    let marker = temp.path().join("plugin-executed");
    fs::write(temp.path().join(".meta.yaml"), "projects: [malformed").unwrap();

    write_executable(
        &plugin_dir.join("meta-suite"),
        r#"#!/bin/sh
if [ "$1" = "--meta-plugin-info" ]; then
  printf '%s\n' '{"name":"suite","version":"1.0.0","commands":["tool"],"bare_help_commands":["tool"],"help":{"usage":"meta tool <command>"}}'
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

    let output = meta_command(temp.path(), &plugin_dir, temp.path())
        .env("META_TEST_MARKER", &marker)
        .arg("tool")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("meta tool <command>"));
    assert!(
        !marker.exists(),
        "declared bare help executed the plugin command"
    );

    fs::remove_file(temp.path().join(".meta.yaml")).unwrap();
    fs::write(temp.path().join(".meta"), r#"{"projects":{}}"#).unwrap();
    write_executable(
        &plugin_dir.join("meta-runner"),
        r#"#!/bin/sh
if [ "$1" = "--meta-plugin-info" ]; then
  printf '%s\n' '{"name":"runner","version":"1.0.0","commands":["action"]}'
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

    let output = meta_command(temp.path(), &plugin_dir, temp.path())
        .env("META_TEST_MARKER", &marker)
        .arg("action")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        marker.exists(),
        "unlisted promoted command was reclassified as help"
    );
}

#[test]
fn separator_payload_and_policy_capability_reach_the_plugin_unchanged() {
    let temp = tempdir().unwrap();
    let plugin_dir = create_plugin_dir(temp.path());
    let request_path = temp.path().join("plugin-request.json");
    fs::write(temp.path().join(".meta"), r#"{"projects":{}}"#).unwrap();

    write_executable(
        &plugin_dir.join("meta-tool"),
        r#"#!/bin/sh
if [ "$1" = "--meta-plugin-info" ]; then
  printf '%s\n' '{"name":"tool","version":"1.0.0","commands":["tool"]}'
  exit 0
fi
if [ "$1" = "--meta-plugin-exec" ]; then
  IFS= read -r request || :
  printf '%s\n' "$request" > "$META_TEST_REQUEST"
  printf '%s\n' '{"plan":{"commands":[]}}'
  exit 0
fi
exit 1
"#,
    );

    let output = meta_command(temp.path(), &plugin_dir, temp.path())
        .env("META_TEST_REQUEST", &request_path)
        .args(["tool", "run", "--", "--help", "--recursive", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let request: serde_json::Value =
        serde_json::from_slice(&fs::read(&request_path).unwrap()).unwrap();
    assert_eq!(request["command"], "tool");
    assert_eq!(
        request["args"],
        serde_json::json!(["run", "--", "--help", "--recursive", "--json"])
    );
    assert_eq!(
        request["host_capabilities"],
        serde_json::json!(["plan-execution-policy-v1"])
    );
    assert_eq!(request["options"]["recursive"], false);
    assert_eq!(request["options"]["json_output"], false);
}

#[test]
fn no_config_worktree_still_dispatches_to_plugins() {
    let temp = tempdir().unwrap();
    let plugin_dir = create_plugin_dir(temp.path());
    let task_dir = temp.path().join(".worktrees").join("synthetic-task");
    let repo_dir = task_dir.join("repo");
    let marker = temp.path().join("plugin-executed");
    fs::create_dir_all(&repo_dir).unwrap();
    fs::write(
        repo_dir.join(".git"),
        format!(
            "gitdir: {}\n",
            temp.path()
                .join("source")
                .join(".git")
                .join("worktrees")
                .join("synthetic-task")
                .display()
        ),
    )
    .unwrap();

    write_executable(
        &plugin_dir.join("meta-tool"),
        r#"#!/bin/sh
if [ "$1" = "--meta-plugin-info" ]; then
  printf '%s\n' '{"name":"tool","version":"1.0.0","commands":["tool"]}'
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

    let output = meta_command(&repo_dir, &plugin_dir, temp.path())
        .env("META_TEST_MARKER", &marker)
        .args(["tool", "run"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(marker.exists(), "worktree command bypassed the plugin");
}

#[test]
fn child_only_plan_uses_the_actual_meta_root_for_output_labels() {
    let temp = tempdir().unwrap();
    let plugin_dir = create_plugin_dir(temp.path());
    let child = temp.path().join("child");
    fs::create_dir(&child).unwrap();
    fs::write(
        temp.path().join(".meta"),
        r#"{"projects":{"child":{"repo":"https://example.invalid/child.git","path":"child"}}}"#,
    )
    .unwrap();

    write_executable(
        &plugin_dir.join("meta-tool"),
        r#"#!/bin/sh
if [ "$1" = "--meta-plugin-info" ]; then
  printf '%s\n' '{"name":"tool","version":"1.0.0","commands":["tool"]}'
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

    let output = meta_command(temp.path(), &plugin_dir, temp.path())
        .env("META_TEST_CHILD", &child)
        .args(["--sequential", "--include", "child", "tool", "check"])
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

#[test]
fn plan_execution_policy_controls_aliases_and_host_filters() {
    let temp = tempdir().unwrap();
    let plugin_dir = create_plugin_dir(temp.path());
    let marker = temp.path().join("policy-marker");
    fs::write(temp.path().join(".meta"), r#"{"projects":{}}"#).unwrap();
    fs::write(
        temp.path().join(".looprc"),
        r#"{"aliases":{"true":"false"}}"#,
    )
    .unwrap();

    write_executable(
        &plugin_dir.join("meta-tool"),
        r#"#!/bin/sh
if [ "$1" = "--meta-plugin-info" ]; then
  printf '%s\n' '{"name":"tool","version":"1.0.0","commands":["tool"]}'
  exit 0
fi
if [ "$1" = "--meta-plugin-exec" ]; then
  IFS= read -r request || :
  printf '{"plan":{"commands":[{"dir":"%s","cmd":"true && printf policy-ok > policy-marker"}],"parallel":false},"execution_policy":{"expand_loop_aliases":false,"apply_host_filters":false}}\n' "$META_TEST_ROOT"
  exit 0
fi
exit 1
"#,
    );

    let output = meta_command(temp.path(), &plugin_dir, temp.path())
        .env("META_TEST_ROOT", temp.path())
        .args(["--sequential", "--include", "does-not-match", "tool", "run"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(marker).unwrap(), "policy-ok");
}
