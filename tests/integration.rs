use assert_cmd::Command;

#[test]
fn test_meta_command_execution() {
    let mut cmd = Command::cargo_bin("meta").unwrap();
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
