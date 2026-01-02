//! Workspace snapshots for safe multi-repo operations with rollback
//!
//! Allows agents to safely modify dozens of repos with the ability to rollback
//! if something goes wrong.
//!
//! # Usage
//!
//! ```bash
//! meta snapshot create "pre-upgrade"
//! # ... make changes ...
//! meta snapshot restore "pre-upgrade"
//! ```

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A snapshot of the entire workspace state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub description: Option<String>,
    pub meta_dir: String,
    pub projects: Vec<ProjectSnapshot>,
}

/// A snapshot of a single project's git state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    pub name: String,
    pub path: String,
    pub branch: String,
    pub commit_hash: String,
    pub is_dirty: bool,
    /// Stash reference if we had to stash dirty changes
    pub stash_ref: Option<String>,
    /// Tracked files with uncommitted changes
    pub dirty_files: Vec<String>,
}

impl WorkspaceSnapshot {
    /// Create a new snapshot of the current workspace state
    pub fn create(
        name: &str,
        meta_dir: &Path,
        projects: &[(String, PathBuf, Vec<String>)], // (name, path, tags)
        description: Option<String>,
    ) -> Result<Self> {
        let mut project_snapshots = Vec::new();

        for (proj_name, proj_path, _tags) in projects {
            if !proj_path.exists() {
                log::warn!("Project path does not exist: {}", proj_path.display());
                continue;
            }

            // Check if it's a git repo
            if !proj_path.join(".git").exists() {
                log::warn!("Not a git repository: {}", proj_path.display());
                continue;
            }

            let snapshot = ProjectSnapshot::capture(proj_name, proj_path)?;
            project_snapshots.push(snapshot);
        }

        Ok(WorkspaceSnapshot {
            name: name.to_string(),
            created_at: Utc::now(),
            description,
            meta_dir: meta_dir.to_string_lossy().to_string(),
            projects: project_snapshots,
        })
    }

    /// Restore the workspace to this snapshot's state
    pub fn restore(&self, force: bool) -> Result<RestoreResult> {
        let mut restored = Vec::new();
        let mut failed = Vec::new();
        let mut skipped = Vec::new();

        for project in &self.projects {
            let path = PathBuf::from(&project.path);
            if !path.exists() {
                skipped.push(RestoreSkipped {
                    project: project.name.clone(),
                    reason: "Path does not exist".to_string(),
                });
                continue;
            }

            match project.restore(&path, force) {
                Ok(()) => {
                    restored.push(project.name.clone());
                }
                Err(e) => {
                    failed.push(RestoreFailed {
                        project: project.name.clone(),
                        error: e.to_string(),
                    });
                }
            }
        }

        Ok(RestoreResult {
            restored,
            failed,
            skipped,
        })
    }

    /// Save snapshot to a file
    pub fn save(&self, snapshots_dir: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(snapshots_dir)?;

        let filename = format!("{}.json", sanitize_filename(&self.name));
        let path = snapshots_dir.join(&filename);

        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;

        Ok(path)
    }

    /// Load snapshot from a file
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read snapshot file: {}", path.display()))?;
        let snapshot: WorkspaceSnapshot = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse snapshot file: {}", path.display()))?;
        Ok(snapshot)
    }
}

impl ProjectSnapshot {
    /// Capture the current state of a project
    pub fn capture(name: &str, path: &Path) -> Result<Self> {
        // Get current branch
        let branch = git_output(path, &["rev-parse", "--abbrev-ref", "HEAD"])?;

        // Get current commit
        let commit_hash = git_output(path, &["rev-parse", "HEAD"])?;

        // Get dirty status
        let status = git_output(path, &["status", "--porcelain"])?;
        let is_dirty = !status.is_empty();

        // Get list of dirty files
        let dirty_files: Vec<String> = status.lines().map(|l| l[3..].to_string()).collect();

        Ok(ProjectSnapshot {
            name: name.to_string(),
            path: path.to_string_lossy().to_string(),
            branch,
            commit_hash,
            is_dirty,
            stash_ref: None,
            dirty_files,
        })
    }

    /// Restore this project to the snapshot state
    pub fn restore(&self, path: &Path, force: bool) -> Result<()> {
        // Check if there are uncommitted changes
        let current_status = git_output(path, &["status", "--porcelain"])?;
        let is_currently_dirty = !current_status.is_empty();

        if is_currently_dirty && !force {
            anyhow::bail!(
                "Project '{}' has uncommitted changes. Use force=true to override.",
                self.name
            );
        }

        // If force and dirty, stash current changes
        if is_currently_dirty && force {
            let _ = git_command(
                path,
                &["stash", "push", "-m", "meta-snapshot-restore-backup"],
            );
        }

        // Checkout the branch
        git_command(path, &["checkout", &self.branch]).with_context(|| {
            format!(
                "Failed to checkout branch '{}' in '{}'",
                self.branch, self.name
            )
        })?;

        // Reset to the commit
        git_command(path, &["reset", "--hard", &self.commit_hash]).with_context(|| {
            format!(
                "Failed to reset to commit '{}' in '{}'",
                self.commit_hash, self.name
            )
        })?;

        // If the original snapshot had a stash, try to apply it
        if let Some(ref _stash_ref) = self.stash_ref {
            // Note: This is best-effort; stash refs may not survive across operations
            let _ = git_command(path, &["stash", "pop"]);
        }

        Ok(())
    }
}

/// Result of a restore operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreResult {
    pub restored: Vec<String>,
    pub failed: Vec<RestoreFailed>,
    pub skipped: Vec<RestoreSkipped>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreFailed {
    pub project: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreSkipped {
    pub project: String,
    pub reason: String,
}

/// Snapshot manager for listing and managing snapshots
pub struct SnapshotManager {
    snapshots_dir: PathBuf,
}

impl SnapshotManager {
    /// Create a new snapshot manager
    pub fn new(meta_dir: &Path) -> Self {
        let snapshots_dir = meta_dir.join(".meta-snapshots");
        SnapshotManager { snapshots_dir }
    }

    /// List all available snapshots
    pub fn list(&self) -> Result<Vec<SnapshotInfo>> {
        if !self.snapshots_dir.exists() {
            return Ok(Vec::new());
        }

        let mut snapshots = Vec::new();

        for entry in std::fs::read_dir(&self.snapshots_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(snapshot) = WorkspaceSnapshot::load(&path) {
                    snapshots.push(SnapshotInfo {
                        name: snapshot.name,
                        created_at: snapshot.created_at,
                        description: snapshot.description,
                        project_count: snapshot.projects.len(),
                        path: path.to_string_lossy().to_string(),
                    });
                }
            }
        }

        // Sort by creation time, newest first
        snapshots.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        Ok(snapshots)
    }

    /// Get a snapshot by name
    pub fn get(&self, name: &str) -> Result<Option<WorkspaceSnapshot>> {
        let filename = format!("{}.json", sanitize_filename(name));
        let path = self.snapshots_dir.join(&filename);

        if path.exists() {
            Ok(Some(WorkspaceSnapshot::load(&path)?))
        } else {
            Ok(None)
        }
    }

    /// Delete a snapshot by name
    pub fn delete(&self, name: &str) -> Result<bool> {
        let filename = format!("{}.json", sanitize_filename(name));
        let path = self.snapshots_dir.join(&filename);

        if path.exists() {
            std::fs::remove_file(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Save a snapshot
    pub fn save(&self, snapshot: &WorkspaceSnapshot) -> Result<PathBuf> {
        snapshot.save(&self.snapshots_dir)
    }

    /// Get the snapshots directory
    pub fn snapshots_dir(&self) -> &Path {
        &self.snapshots_dir
    }
}

/// Summary information about a snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotInfo {
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub description: Option<String>,
    pub project_count: usize,
    pub path: String,
}

/// Atomic batch execution with rollback
#[derive(Debug)]
pub struct AtomicBatch {
    /// Snapshot taken before execution
    pre_snapshot: Option<WorkspaceSnapshot>,
    /// Projects involved in this batch
    projects: Vec<(String, PathBuf)>,
    /// Whether to auto-rollback on failure
    auto_rollback: bool,
}

impl AtomicBatch {
    /// Create a new atomic batch
    pub fn new(
        meta_dir: &Path,
        projects: Vec<(String, PathBuf, Vec<String>)>,
        auto_rollback: bool,
    ) -> Result<Self> {
        // Create a snapshot before execution
        let pre_snapshot = WorkspaceSnapshot::create(
            &format!("atomic-batch-{}", Utc::now().timestamp()),
            meta_dir,
            &projects,
            Some("Automatic snapshot before atomic batch execution".to_string()),
        )?;

        let project_paths: Vec<(String, PathBuf)> = projects
            .into_iter()
            .map(|(name, path, _)| (name, path))
            .collect();

        Ok(AtomicBatch {
            pre_snapshot: Some(pre_snapshot),
            projects: project_paths,
            auto_rollback,
        })
    }

    /// Execute a command across all projects
    /// Returns results and automatically rolls back on failure if configured
    pub fn execute(&self, command: &str) -> Result<BatchExecutionResult> {
        let mut results = Vec::new();
        let mut has_failure = false;

        for (name, path) in &self.projects {
            if !path.exists() {
                results.push(ProjectExecutionResult {
                    project: name.clone(),
                    success: false,
                    stdout: String::new(),
                    stderr: format!("Path does not exist: {}", path.display()),
                });
                has_failure = true;
                continue;
            }

            let output = Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(path)
                .output();

            match output {
                Ok(out) => {
                    let success = out.status.success();
                    if !success {
                        has_failure = true;
                    }
                    results.push(ProjectExecutionResult {
                        project: name.clone(),
                        success,
                        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
                        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
                    });
                }
                Err(e) => {
                    has_failure = true;
                    results.push(ProjectExecutionResult {
                        project: name.clone(),
                        success: false,
                        stdout: String::new(),
                        stderr: e.to_string(),
                    });
                }
            }

            // If auto-rollback and we have a failure, stop and rollback
            if has_failure && self.auto_rollback {
                break;
            }
        }

        let mut rollback_result = None;
        if has_failure && self.auto_rollback {
            if let Some(ref snapshot) = self.pre_snapshot {
                rollback_result = Some(snapshot.restore(true)?);
            }
        }

        Ok(BatchExecutionResult {
            results,
            has_failure,
            rolled_back: rollback_result.is_some(),
            rollback_result,
        })
    }

    /// Get the pre-execution snapshot
    pub fn snapshot(&self) -> Option<&WorkspaceSnapshot> {
        self.pre_snapshot.as_ref()
    }
}

/// Result of a batch execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchExecutionResult {
    pub results: Vec<ProjectExecutionResult>,
    pub has_failure: bool,
    pub rolled_back: bool,
    pub rollback_result: Option<RestoreResult>,
}

/// Result of executing a command in a single project
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectExecutionResult {
    pub project: String,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

// Helper functions

fn git_output(path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .with_context(|| format!("Failed to run git {args:?}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        anyhow::bail!(
            "Git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

fn git_command(path: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .with_context(|| format!("Failed to run git {args:?}"))?;

    if output.status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "Git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_test_repo(dir: &Path) -> Result<()> {
        git_command(dir, &["init"])?;
        git_command(dir, &["config", "user.email", "test@test.com"])?;
        git_command(dir, &["config", "user.name", "Test"])?;

        // Create a file and commit
        std::fs::write(dir.join("test.txt"), "hello")?;
        git_command(dir, &["add", "."])?;
        git_command(dir, &["commit", "-m", "Initial commit"])?;

        Ok(())
    }

    #[test]
    fn test_project_snapshot_capture() {
        let temp_dir = TempDir::new().unwrap();
        setup_test_repo(temp_dir.path()).unwrap();

        let snapshot = ProjectSnapshot::capture("test", temp_dir.path()).unwrap();

        assert_eq!(snapshot.name, "test");
        assert!(!snapshot.commit_hash.is_empty());
        assert!(!snapshot.is_dirty);
    }

    #[test]
    fn test_project_snapshot_dirty() {
        let temp_dir = TempDir::new().unwrap();
        setup_test_repo(temp_dir.path()).unwrap();

        // Make a change without committing
        std::fs::write(temp_dir.path().join("test.txt"), "modified").unwrap();

        let snapshot = ProjectSnapshot::capture("test", temp_dir.path()).unwrap();

        assert!(snapshot.is_dirty);
        assert!(!snapshot.dirty_files.is_empty());
    }

    #[test]
    fn test_snapshot_save_load() {
        let temp_dir = TempDir::new().unwrap();
        let snapshots_dir = temp_dir.path().join("snapshots");

        let snapshot = WorkspaceSnapshot {
            name: "test-snapshot".to_string(),
            created_at: Utc::now(),
            description: Some("Test description".to_string()),
            meta_dir: "/test".to_string(),
            projects: vec![],
        };

        let path = snapshot.save(&snapshots_dir).unwrap();
        assert!(path.exists());

        let loaded = WorkspaceSnapshot::load(&path).unwrap();
        assert_eq!(loaded.name, "test-snapshot");
        assert_eq!(loaded.description, Some("Test description".to_string()));
    }

    #[test]
    fn test_snapshot_manager_list() {
        let temp_dir = TempDir::new().unwrap();
        let manager = SnapshotManager::new(temp_dir.path());

        // Initially empty
        let snapshots = manager.list().unwrap();
        assert!(snapshots.is_empty());

        // Create and save a snapshot
        let snapshot = WorkspaceSnapshot {
            name: "test".to_string(),
            created_at: Utc::now(),
            description: None,
            meta_dir: temp_dir.path().to_string_lossy().to_string(),
            projects: vec![],
        };
        manager.save(&snapshot).unwrap();

        // Should now have one snapshot
        let snapshots = manager.list().unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].name, "test");
    }

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("my-snapshot"), "my-snapshot");
        assert_eq!(sanitize_filename("my snapshot"), "my_snapshot");
        assert_eq!(sanitize_filename("test/name"), "test_name");
        assert_eq!(sanitize_filename("pre:upgrade"), "pre_upgrade");
    }
}
