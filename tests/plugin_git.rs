/*
use std::fs;
use std::env;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_meta_git_clone_invalid_repo() {
    let dir = tempdir().unwrap();
    let meta_path = dir.path().join(".meta");
    fs::write(
        &meta_path,
        r#"{
            "projects": {}
        }"#,
    )
    .unwrap();

    let output = Command::new(env::var("CARGO_BIN_EXE_meta_cli").expect("CARGO_BIN_EXE_meta_cli not set"))
        .current_dir(dir.path())
        .args(&["git", "clone", "invalid_repo_url"])
        .output()
        .expect("failed to execute meta git clone");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal") || !output.status.success(),
        "Expected git clone to fail on invalid repo"
    );
#[test]
fn test_meta_git_clone_success_local_repo() {
    let dir = tempdir().unwrap();
    let repo_dir = dir.path().join("origin_repo");
    let clone_dir = dir.path().join("cloned_repo");

    // Initialize a bare git repo to clone
    std::fs::create_dir_all(&repo_dir).unwrap();
    let init_output = Command::new("git")
        .arg("init")
        .arg("--bare")
        .arg(&repo_dir)
        .output()
        .expect("failed to init bare repo");
    assert!(init_output.status.success());

    let meta_path = dir.path().join(".meta");
    std::fs::write(
        &meta_path,
        r#"{
            "projects": {}
        }"#,
    )
    .unwrap();

    let output = Command::new(env::var("CARGO_BIN_EXE_meta_cli").expect("CARGO_BIN_EXE_meta_cli not set"))
        .current_dir(dir.path())
        .args(&["git", "clone", repo_dir.to_str().unwrap(), clone_dir.to_str().unwrap()])
        .output()
        .expect("failed to execute meta git clone");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Expected git clone to succeed, stderr: {}, stdout: {}", stderr, stdout
    );
    assert!(clone_dir.exists());
}
}
*/
