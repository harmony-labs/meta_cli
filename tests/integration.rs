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
