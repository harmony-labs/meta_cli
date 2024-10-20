use assert_cmd::Command;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_meta_command_execution() {
    let temp_dir = tempdir().unwrap();
    let meta_file_path = temp_dir.path().join(".meta");
    let meta_content = r#"
    {
        "projects": {
            "project1": "path/to/project1",
            "project2": "path/to/project2"
        },
        "ignore": [".git"]
    }
    "#;
    fs::write(&meta_file_path, meta_content).unwrap();

    let mut cmd = Command::cargo_bin("meta").unwrap();
    cmd.arg("--config")
        .arg(meta_file_path)
        .arg("echo")
        .arg("test")
        .assert()
        .success()
        .stdout(predicates::str::contains("test"));
}
