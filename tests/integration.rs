use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_meta_command_execution() {
    let temp_dir = TempDir::new().unwrap();
    let meta_path = temp_dir.path().join(".meta");
    let meta_content = r#"
    {
        "ignore": [".git"],
        "projects": {
            "dir1": "./dir1",
            "dir2": "./dir2"
        }
    }
    "#;
    fs::write(&meta_path, meta_content).unwrap();

    let temp_dir = TempDir::new().unwrap();
    let meta_path = temp_dir.path().join(".meta");
    let meta_content = r#"
    {
        "ignore": [".git"],
        "projects": {
            "dir1": "./dir1",
            "dir2": "./dir2"
        }
    }
    "#;
    fs::write(&meta_path, meta_content).unwrap();

    let temp_dir = TempDir::new().unwrap();
    let meta_path = temp_dir.path().join(".meta");
    let meta_content = r#"
    {
        "ignore": [".git"],
        "projects": {
            "dir1": "./dir1",
            "dir2": "./dir2"
        }
    }
    "#;
    fs::write(&meta_path, meta_content).unwrap();

    let temp_dir = TempDir::new().unwrap();
    let meta_path = temp_dir.path().join(".meta");
    let meta_content = r#"
    {
        "ignore": [".git"],
        "projects": {
            "dir1": "./dir1",
            "dir2": "./dir2"
        }
    }
    "#;
    fs::write(&meta_path, meta_content).unwrap();

    let temp_dir = TempDir::new().unwrap();
    let meta_path = temp_dir.path().join(".meta");
    let meta_content = r#"
    {
        "ignore": [".git"],
        "projects": {
            "dir1": "./dir1",
            "dir2": "./dir2"
        }
    }
    "#;
    fs::write(&meta_path, meta_content).unwrap();

    let mut cmd = Command::cargo_bin("meta").unwrap();
    cmd.current_dir(temp_dir.path());
    cmd.current_dir(temp_dir.path());
    cmd.current_dir(temp_dir.path());
    cmd.current_dir(temp_dir.path());
    cmd.current_dir(temp_dir.path());
    let dir = std::env::current_dir().unwrap().join("tests/examples");
    cmd.current_dir(dir)
        .arg("echo")
        .arg("test")
        .assert()
        .success()
        .stdout(predicates::str::contains("test"));
}

#[test]
fn test_meta_with_include_option() {
    let mut cmd = Command::cargo_bin("meta").unwrap();
    cmd.arg("--include")
        .arg("dir1")
        .arg("echo")
        .arg("test")
        .assert()
        .success();
}

#[test]
fn test_meta_with_exclude_option() {
    let mut cmd = Command::cargo_bin("meta").unwrap();
    cmd.arg("--exclude")
        .arg("dir1")
        .arg("echo")
        .arg("test")
        .assert()
        .success();
}

#[test]
fn test_meta_with_silent_option() {
    let mut cmd = Command::cargo_bin("meta").unwrap();
    cmd.arg("--silent")
        .arg("echo")
        .arg("test")
        .assert()
        .success();
}

#[test]
fn test_meta_with_parallel_option() {
    let mut cmd = Command::cargo_bin("meta").unwrap();
    cmd.arg("--parallel")
        .arg("echo")
        .arg("test")
        .assert()
        .success();
}

#[test]
fn test_meta_with_add_aliases_option() {
    let mut cmd = Command::cargo_bin("meta").unwrap();
    cmd.arg("--add-aliases-to-global-looprc")
        .arg("echo")
        .arg("test")
        .assert()
        .success();
}

// #[test]
// fn test_meta_command_execution_with_non_existent_directory() {
//     let mut cmd = Command::cargo_bin("meta").unwrap();
//     println!("current_dir: {}", std::env::current_dir().unwrap().display());
//     let dir = std::env::current_dir().unwrap().join("tests/examples");
//     cmd.current_dir(dir)
//         .arg("-c .meta2")
//         .arg("echo")
//         .arg("test")
//         .assert()
//         .success()
//         .stdout(predicates::str::contains("test"));
// }
