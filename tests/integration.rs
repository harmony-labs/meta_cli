use assert_cmd::Command;

// AI INSTRUCTION: ENSURE ALL OTHER TESTS CONTINUE TO PASS, WHEN ADDING NEW TESTS

#[test]
fn test_meta_command_execution() {
    let meta_file_path = ".meta";

    let mut cmd = Command::cargo_bin("meta").unwrap();
    cmd.current_dir("tests/examples")
        .arg("--config")
        .arg(meta_file_path)
        .arg("echo")
        .arg("test")
        .assert()
        .success()
        .stdout(predicates::str::contains("test"));
}

#[test]
fn test_meta_with_include_option() {
    let meta_file_path = ".meta";

    let mut cmd = Command::cargo_bin("meta").unwrap();
    cmd.current_dir("tests/examples")
        .arg("--config")
        .arg(meta_file_path)
        .arg("--include")
        .arg("dir1")
        .arg("echo")
        .arg("test")
        .assert()
        .success();
}

#[test]
fn test_meta_with_exclude_option() {
    let meta_file_path = ".meta";

    let mut cmd = Command::cargo_bin("meta").unwrap();
    cmd.current_dir("tests/examples")
        .arg("--config")
        .arg(meta_file_path)
        .arg("--exclude")
        .arg("dir1")
        .arg("echo")
        .arg("test")
        .assert()
        .success();
}

#[test]
fn test_meta_with_silent_option() {
    let meta_file_path = ".meta";

    let mut cmd = Command::cargo_bin("meta").unwrap();
    cmd.current_dir("tests/examples")
        .arg("--config")
        .arg(meta_file_path)
        .arg("--silent")
        .arg("echo")
        .arg("test")
        .assert()
        .success();
}

#[test]
fn test_meta_with_parallel_option() {
    let meta_file_path = ".meta";

    let mut cmd = Command::cargo_bin("meta").unwrap();
    cmd.current_dir("tests/examples")
        .arg("--config")
        .arg(meta_file_path)
        .arg("echo")
        .arg("test")
        .assert()
        .success();
}

#[test]
fn test_meta_with_add_aliases_option() {
    let meta_file_path = ".meta";

    let mut cmd = Command::cargo_bin("meta").unwrap();
    cmd.current_dir("tests/examples")
        .arg("--config")
        .arg(meta_file_path)
        .arg("--add-aliases-to-global-looprc")
        .arg("echo")
        .arg("test")
        .assert()
        .success();
}

#[test]
fn test_meta_command_execution_with_non_existent_directory() {
    let meta_file_path = ".meta2";

    let mut cmd = Command::cargo_bin("meta").unwrap();
    cmd.current_dir("tests/examples")
        .arg("--config")
        .arg(meta_file_path)
        .arg("echo")
        .arg("test")
        .assert()
        .failure()
        .stderr(predicates::str::contains("Error: At least one command failed"))
        .stdout(predicates::str::contains("✗ project3: No directory found. Command: echo test (Exit code: 1)"))
        .stdout(predicates::str::contains("Summary: ✗ 1 out of 4 commands failed"));
}
