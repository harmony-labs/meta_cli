use std::fs;
use std::env;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_meta_cargo_build_skips_non_rust() {
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

    // Only repo2 has Cargo.toml
    fs::write(dir.path().join("repo2").join("Cargo.toml"), "[package]\nname = \"dummy\"\nversion = \"0.1.0\"").unwrap();

    let output = Command::new(std::env::var("CARGO_BIN_EXE_meta_cli").expect("CARGO_BIN_EXE_meta_cli not set"))
        .current_dir(dir.path())
        .args(&["cargo", "build"])
        .output()
        .expect("failed to execute meta cargo build");

    let stdout = String::from_utf8_lossy(&output.stdout);
#[test]
fn test_meta_cargo_build_failure() {
    let dir = tempdir().unwrap();
    let meta_path = dir.path().join(".meta");
    fs::write(
        &meta_path,
        r#"{
            "projects": {
                "bad_repo": "./bad_repo"
            }
        }"#,
    )
    .unwrap();

    let bad_repo = dir.path().join("bad_repo");
    fs::create_dir_all(&bad_repo).unwrap();
    // Write invalid Cargo.toml to cause build failure
    fs::write(bad_repo.join("Cargo.toml"), "invalid content").unwrap();

    let output = Command::new(std::env::var("CARGO_BIN_EXE_meta_cli").expect("CARGO_BIN_EXE_meta_cli not set"))
        .current_dir(dir.path())
        .args(&["cargo", "build"])
        .output()
        .expect("failed to execute meta cargo build");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error") || !output.status.success(),
        "Expected cargo build to fail on invalid Cargo.toml"
    );
}
    assert!(stdout.contains("Skipping: no Cargo.toml") || output.status.success());
}