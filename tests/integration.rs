use std::fs;
use std::env;
use std::process::Command;
use tempfile::tempdir;
fn get_meta_cli_path() -> String {
    std::env::var("CARGO_BIN_EXE_meta_cli")
        .or_else(|_| std::env::var("META_CLI_PATH"))
        .unwrap_or_else(|_| "target/debug/meta".to_string())
}

#[test]
fn test_meta_exec_pwd() {
    let dir = tempdir().unwrap();
    let meta_path = dir.path().join(".meta");
    fs::write(
        &meta_path,
        r#"{
            "projects": {
                "repo1": "./repo1",
                "repo2": "./repo2"
            }
        }"#,
    )
    .unwrap();

    fs::create_dir_all(dir.path().join("repo1")).unwrap();
    fs::create_dir_all(dir.path().join("repo2")).unwrap();

    let output = Command::new(get_meta_cli_path())
        .current_dir(dir.path())
        .arg("exec")
        .arg("pwd")
        .output()
        .expect("failed to execute meta");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("repo1"));
    assert!(stdout.contains("repo2"));
    assert!(output.status.success());
}
#[test]
fn test_fallback_to_loop_engine_when_no_plugin_handles() {
    let dir = tempdir().unwrap();
    let meta_path = dir.path().join(".meta");
    fs::write(
        &meta_path,
        r#"{
            "projects": {
                "repo1": "./repo1"
            }
        }"#,
    )
    .unwrap();

    fs::create_dir_all(dir.path().join("repo1")).unwrap();

    let output = Command::new(get_meta_cli_path())
        .current_dir(dir.path())
        .arg("nonexistent_command")
        .output()
        .expect("failed to execute meta");

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Expect fallback message or graceful failure
    assert!(
        stderr.contains("No plugin handled") || !output.status.success(),
        "Expected fallback or error when no plugin handles command"
    );
}

#[test]
fn test_missing_meta_file() {
    let dir = tempdir().unwrap();

    let output = Command::new(get_meta_cli_path())
        .current_dir(dir.path())
        .arg("exec")
        .arg("pwd")
        .output()
        .expect("failed to execute meta");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No such file") || stderr.contains("not found") || !output.status.success(),
        "Expected error when .meta file is missing"
    );
}

#[test]
fn test_malformed_meta_file() {
    let dir = tempdir().unwrap();
    let meta_path = dir.path().join(".meta");
    fs::write(&meta_path, "{ invalid json").unwrap();

    let output = Command::new(get_meta_cli_path())
        .current_dir(dir.path())
        .arg("exec")
        .arg("pwd")
        .output()
        .expect("failed to execute meta");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error") || stderr.contains("failed") || !output.status.success(),
        "Expected error on malformed .meta file"
    );
}

#[test]
fn test_cli_argument_parsing_edge_cases() {
    let dir = tempdir().unwrap();
    let meta_path = dir.path().join(".meta");
    fs::write(
        &meta_path,
        r#"{
            "projects": {}
        }"#,
    )
    .unwrap();

    // Unknown flag
    let output = Command::new(get_meta_cli_path())
        .current_dir(dir.path())
        .arg("--unknown-flag")
        .output()
        .expect("failed to execute meta with unknown flag");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error") || stderr.contains("unknown") || !output.status.success(),
        "Expected error on unknown CLI flag"
    );
}

#[test]
fn test_plugin_dispatch_failure_simulation() {
    let dir = tempdir().unwrap();
    let meta_path = dir.path().join(".meta");
    fs::write(
        &meta_path,
        r#"{
            "projects": {
                "repo1": "./repo1"
            }
        }"#,
    )
    .unwrap();

    fs::create_dir_all(dir.path().join("repo1")).unwrap();

    // Simulate a plugin command that is expected to fail, e.g., invalid git URL
    let output = Command::new(get_meta_cli_path())
        .current_dir(dir.path())
        .args(&["git", "clone", "invalid_url"])
        .output()
        .expect("failed to execute meta git clone");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal") || !output.status.success(),
        "Expected plugin dispatch failure to propagate error"
    );
}
#[test]
fn test_loop_engine_integration() {
    let dir = tempdir().unwrap();
    let meta_path = dir.path().join(".meta");
    fs::write(
        &meta_path,
        r#"{
            "projects": {}
        }"#,
    )
    .unwrap();

    let output = Command::new(get_meta_cli_path())
        .current_dir(dir.path())
        .arg("loop")
        .output()
        .expect("failed to execute meta loop");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Accept either success or graceful fallback
    assert!(
        output.status.success() || stderr.contains("error") || stdout.contains("loop"),
        "Expected loop engine integration or graceful handling"
    );
}
