//! Worktree context detection and discovery for meta.
//!
//! Provides filesystem-based detection of worktree contexts and
//! discovery of repos within a worktree set. Used by meta_cli for
//! automatic worktree-scoped command dispatch, and by meta_git_lib
//! for worktree management commands.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::git_utils;

/// Discovered information about a repo within a worktree set.
#[derive(Debug, Clone, Serialize)]
pub struct WorktreeRepoInfo {
    pub alias: String,
    pub branch: String,
    pub path: PathBuf,
    pub source_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_branch: Option<bool>,
}

/// Detect if cwd is inside a `.worktrees/<name>/` directory.
/// Returns (task_name, task_dir, repo_paths) if inside a worktree, None otherwise.
/// Filesystem-based detection — no store dependency.
///
/// Walks the path components looking for a `.worktrees` segment followed by a task name.
/// Works whether cwd is the task dir itself, a repo within it, or deeper inside a repo.
pub fn detect_worktree_context(cwd: &Path) -> Option<(String, PathBuf, Vec<PathBuf>)> {
    use std::path::Component;

    let components: Vec<_> = cwd.components().collect();

    // Find the `.worktrees` component and extract the task name (next component)
    for (i, comp) in components.iter().enumerate() {
        if let Component::Normal(name) = comp {
            if name.to_str() == Some(".worktrees") {
                // Next component after .worktrees is the task name
                let task_name = match components.get(i + 1) {
                    Some(Component::Normal(n)) => n.to_str()?.to_string(),
                    _ => return None, // .worktrees/ with nothing after it
                };

                // Reconstruct the task directory: everything up to and including .worktrees/<task>
                let task_dir: PathBuf = components[..=i + 1].iter().collect();
                let repos = discover_worktree_repos(&task_dir).ok()?;
                if repos.is_empty() {
                    return None;
                }
                let paths = repos.iter().map(|r| r.path.clone()).collect();
                return Some((task_name, task_dir, paths));
            }
        }
    }

    None
}

/// Discover repos within a worktree task directory by scanning for .git files.
/// Results are sorted by alias for deterministic output.
pub fn discover_worktree_repos(task_dir: &Path) -> Result<Vec<WorktreeRepoInfo>> {
    let mut repos = Vec::new();

    // Check if the task dir itself is a worktree (the "." alias)
    let dot_git = task_dir.join(".git");
    if dot_git
        .symlink_metadata()
        .map(|m| m.is_file())
        .unwrap_or(false)
    {
        let source = source_repo_from_gitfile(&dot_git)?;
        let branch = git_utils::current_branch(task_dir).unwrap_or_else(|| "HEAD".to_string());
        repos.push(WorktreeRepoInfo {
            alias: ".".to_string(),
            branch,
            path: task_dir.to_path_buf(),
            source_path: source,
            created_branch: None,
        });
    }

    // Recursively scan subdirectories for worktrees.
    // Repos may be nested (e.g., "vendor/tree-sitter-markdown") when
    // dependencies have paths with intermediate directories.
    if task_dir.is_dir() {
        discover_repos_recursive(task_dir, task_dir, &mut repos)?;
    }

    // Sort by alias for deterministic output ("." sorts first)
    repos.sort_by(|a, b| a.alias.cmp(&b.alias));

    Ok(repos)
}

/// Recursively scan `dir` for git worktree repos, recording aliases as
/// relative paths from `root` (the worktree task directory).
///
/// A directory with a `.git` file is a worktree repo — record it and stop
/// recursing into it. A directory without `.git` is an intermediate directory
/// (e.g., `vendor/`) — recurse into it to find nested repos.
fn discover_repos_recursive(
    root: &Path,
    dir: &Path,
    repos: &mut Vec<WorktreeRepoInfo>,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            log::debug!("Skipping unreadable directory {}: {e}", dir.display());
            return Ok(());
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let sub_path = entry.path();
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }

        // Skip hidden directories
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }

        let sub_git = sub_path.join(".git");
        if sub_git
            .symlink_metadata()
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            // This is a worktree repo — use relative path from root as alias
            let source = match source_repo_from_gitfile(&sub_git) {
                Ok(s) => s,
                Err(e) => {
                    log::debug!("Skipping {}: {e}", sub_path.display());
                    continue;
                }
            };
            let branch =
                git_utils::current_branch(&sub_path).unwrap_or_else(|| "HEAD".to_string());
            let alias = sub_path
                .strip_prefix(root)
                .unwrap_or(&sub_path)
                .to_string_lossy()
                .to_string();
            repos.push(WorktreeRepoInfo {
                alias,
                branch,
                path: sub_path,
                source_path: source,
                created_branch: None,
            });
        } else {
            // Not a repo — recurse into intermediate directory
            if let Err(e) = discover_repos_recursive(root, &sub_path, repos) {
                log::debug!("Skipping subtree {}: {e}", sub_path.display());
            }
        }
    }
    Ok(())
}

/// Parse a .git file to find the primary checkout path.
/// .git file contains: "gitdir: /path/to/primary/.git/worktrees/<name>"
fn source_repo_from_gitfile(git_file: &Path) -> Result<PathBuf> {
    let content = std::fs::read_to_string(git_file)
        .with_context(|| format!("Failed to read .git file at {}", git_file.display()))?;

    let gitdir = content
        .trim()
        .strip_prefix("gitdir: ")
        .ok_or_else(|| anyhow::anyhow!("Invalid .git file format at {}", git_file.display()))?;

    // gitdir points to: /path/to/primary/.git/worktrees/<name>
    // We need: /path/to/primary/
    let gitdir_path = PathBuf::from(gitdir);
    // Walk up: worktrees/<name> -> .git -> repo root
    let dot_git_dir = gitdir_path
        .parent() // strip worktree name
        .and_then(|p| p.parent()) // strip "worktrees"
        .ok_or_else(|| anyhow::anyhow!("Cannot derive source repo from gitdir: {gitdir}"))?;

    // dot_git_dir is now the .git directory; parent is the repo root
    let repo_root = dot_git_dir.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "Cannot find repo root from .git dir: {}",
            dot_git_dir.display()
        )
    })?;

    Ok(repo_root.to_path_buf())
}
