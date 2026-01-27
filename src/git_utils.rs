//! Shared git primitives used across crates.
//!
//! Lightweight functions that shell out to `git` for common queries.
//! All functions gracefully handle missing repos or git failures.

use std::path::Path;
use std::process::{Command, Stdio};

/// Returns the current branch name, or `None` for detached HEAD / git failures.
pub fn current_branch(repo_path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(repo_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        None // detached HEAD
    } else {
        Some(branch)
    }
}

/// Returns whether the repo has uncommitted changes, or `None` if git fails.
pub fn is_dirty(repo_path: &Path) -> Option<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.lines().any(|l| !l.is_empty()))
}

/// Returns the number of dirty files (modified + untracked), or `None` if git fails.
pub fn dirty_file_count(repo_path: &Path) -> Option<usize> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.lines().filter(|l| !l.is_empty()).count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    fn init_git_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        tmp
    }

    fn make_initial_commit(repo: &Path) {
        std::fs::write(repo.join("README.md"), "init\n").unwrap();
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(repo)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
    }

    #[test]
    fn current_branch_returns_branch_name() {
        let tmp = init_git_repo();
        make_initial_commit(tmp.path());
        let branch = current_branch(tmp.path());
        assert!(branch.is_some());
        // Default branch is "main" or "master" depending on git config
        let name = branch.unwrap();
        assert!(!name.is_empty());
    }

    #[test]
    fn current_branch_returns_none_for_nonexistent_path() {
        let result = current_branch(Path::new("/nonexistent/path"));
        assert!(result.is_none());
    }

    #[test]
    fn is_dirty_clean_repo() {
        let tmp = init_git_repo();
        make_initial_commit(tmp.path());
        assert_eq!(is_dirty(tmp.path()), Some(false));
    }

    #[test]
    fn is_dirty_with_modification() {
        let tmp = init_git_repo();
        make_initial_commit(tmp.path());
        std::fs::write(tmp.path().join("README.md"), "changed\n").unwrap();
        assert_eq!(is_dirty(tmp.path()), Some(true));
    }

    #[test]
    fn is_dirty_with_untracked_file() {
        let tmp = init_git_repo();
        make_initial_commit(tmp.path());
        std::fs::write(tmp.path().join("new.txt"), "new").unwrap();
        assert_eq!(is_dirty(tmp.path()), Some(true));
    }

    #[test]
    fn dirty_file_count_clean_repo() {
        let tmp = init_git_repo();
        make_initial_commit(tmp.path());
        assert_eq!(dirty_file_count(tmp.path()), Some(0));
    }

    #[test]
    fn dirty_file_count_mixed() {
        let tmp = init_git_repo();
        make_initial_commit(tmp.path());
        std::fs::write(tmp.path().join("README.md"), "changed\n").unwrap();
        std::fs::write(tmp.path().join("new.txt"), "new").unwrap();
        assert_eq!(dirty_file_count(tmp.path()), Some(2));
    }

    #[test]
    fn dirty_file_count_nonexistent_path() {
        assert!(dirty_file_count(Path::new("/nonexistent/path")).is_none());
    }
}
