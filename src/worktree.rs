//! Multi-repo worktree management for meta.
//!
//! Provides `meta worktree` subcommands for creating, managing, and executing
//! commands across isolated git worktree sets.

use anyhow::{Context, Result};
use colored::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use meta_cli::config;

// ==================== Types ====================

/// Discovered information about a repo within a worktree set.
#[derive(Debug, Clone, Serialize)]
struct WorktreeRepoInfo {
    alias: String,
    branch: String,
    path: PathBuf,
    source_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_branch: Option<bool>,
}

// ==================== JSON Output Structures ====================

#[derive(Debug, Serialize)]
struct CreateOutput {
    name: String,
    root: String,
    repos: Vec<CreateRepoEntry>,
}

#[derive(Debug, Serialize)]
struct CreateRepoEntry {
    alias: String,
    path: String,
    branch: String,
    created_branch: bool,
}

#[derive(Debug, Serialize)]
struct ListOutput {
    worktrees: Vec<ListEntry>,
}

#[derive(Debug, Serialize)]
struct AddOutput {
    name: String,
    repos: Vec<CreateRepoEntry>,
}

#[derive(Debug, Serialize)]
struct ListEntry {
    name: String,
    root: String,
    has_meta_root: bool,
    repos: Vec<ListRepoEntry>,
}

#[derive(Debug, Serialize)]
struct ListRepoEntry {
    alias: String,
    branch: String,
    dirty: bool,
}

#[derive(Debug, Serialize)]
struct StatusOutput {
    name: String,
    repos: Vec<StatusRepoEntry>,
}

#[derive(Debug, Serialize)]
struct StatusRepoEntry {
    alias: String,
    path: String,
    branch: String,
    dirty: bool,
    modified_count: usize,
    untracked_count: usize,
    ahead: u32,
    behind: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    modified_files: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DiffOutput {
    name: String,
    base: String,
    repos: Vec<DiffRepoEntry>,
    totals: DiffTotals,
}

#[derive(Debug, Serialize)]
struct DiffRepoEntry {
    alias: String,
    base_ref: String,
    files_changed: usize,
    insertions: usize,
    deletions: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    files: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DiffTotals {
    repos_changed: usize,
    files_changed: usize,
    insertions: usize,
    deletions: usize,
}

// ==================== Entry Point ====================

pub fn handle_worktree_command(args: &[String], verbose: bool, json: bool) -> Result<()> {
    if args.is_empty() {
        print_help();
        return Ok(());
    }

    // --json may end up in trailing args due to clap's trailing_var_arg
    let json = json || args.iter().any(|a| a == "--json");

    match args[0].as_str() {
        "create" => handle_create(&args[1..], verbose, json),
        "add" => handle_add(&args[1..], verbose, json),
        "destroy" => handle_destroy(&args[1..], verbose, json),
        "list" => handle_list(&args[1..], verbose, json),
        "status" => handle_status(&args[1..], verbose, json),
        "diff" => handle_diff(&args[1..], verbose, json),
        "exec" => handle_exec(&args[1..], verbose, json),
        "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("Unknown worktree command: '{other}'");
            eprintln!("Run 'meta worktree --help' for usage.");
            std::process::exit(1);
        }
    }
}

fn print_help() {
    println!("meta worktree - Multi-repo worktree management");
    println!();
    println!("{}:", "USAGE".bold());
    println!("  meta worktree <command> [options]");
    println!();
    println!("{}:", "COMMANDS".bold());
    println!("  create <name>    Create a new worktree set");
    println!("  add <name>       Add a repo to an existing worktree set");
    println!("  list             List all worktree sets");
    println!("  status <name>    Show detailed status of a worktree set");
    println!("  diff <name>      Show cross-repo diff vs base branch");
    println!("  exec <name>      Run a command across worktree repos");
    println!("  destroy <name>   Remove a worktree set");
    println!();
    println!("{}:", "EXAMPLES".bold());
    println!("  meta worktree create auth-fix --repo core --repo meta_cli");
    println!("  meta worktree create full-task --all");
    println!("  meta worktree exec auth-fix -- cargo test");
    println!("  meta worktree diff auth-fix --stat");
    println!("  meta worktree destroy auth-fix");
}

// ==================== Helpers ====================

fn validate_worktree_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("Worktree name cannot be empty");
    }
    if name.starts_with('.') {
        anyhow::bail!(
            "Invalid worktree name '{}': cannot start with '.'",
            name
        );
    }
    if name.contains('/') || name.contains('\\') {
        anyhow::bail!(
            "Invalid worktree name '{}': cannot contain path separators",
            name
        );
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!(
            "Invalid worktree name '{}': only alphanumeric characters, hyphens, and underscores allowed",
            name
        );
    }
    Ok(())
}

fn resolve_worktree_root(meta_dir: Option<&Path>) -> Result<PathBuf> {
    // 1. Check META_WORKTREES env var
    if let Ok(env_path) = std::env::var("META_WORKTREES") {
        return Ok(PathBuf::from(env_path));
    }
    // 2. Check worktrees_dir in .meta config
    if let Some(dir) = meta_dir {
        if let Some(configured) = read_worktrees_dir_from_config(dir) {
            return Ok(dir.join(configured));
        }
        // 3. Default: .worktrees/ relative to meta root
        return Ok(dir.join(".worktrees"));
    }
    // Fallback if no meta dir found
    let cwd = std::env::current_dir()?;
    Ok(cwd.join(".worktrees"))
}

fn read_worktrees_dir_from_config(meta_dir: &Path) -> Option<String> {
    for name in &[".meta", ".meta.yaml", ".meta.yml"] {
        let path = meta_dir.join(name);
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                // Try JSON
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(dir) = val.get("worktrees_dir").and_then(|v| v.as_str()) {
                        return Some(dir.to_string());
                    }
                }
                // Try YAML
                if let Ok(val) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
                    if let Some(dir) = val.get("worktrees_dir").and_then(|v| v.as_str()) {
                        return Some(dir.to_string());
                    }
                }
            }
        }
    }
    None
}

fn find_meta_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    config::find_meta_config(&cwd, None)
        .map(|(path, _)| path.parent().unwrap_or(Path::new(".")).to_path_buf())
}

fn resolve_branch(task_name: &str, branch_flag: Option<&str>, per_repo_branch: Option<&str>) -> String {
    per_repo_branch
        .or(branch_flag)
        .map(|s| s.to_string())
        .unwrap_or_else(|| task_name.to_string())
}

/// Parse --repo arguments: --repo alias[:branch]
fn parse_repo_args(args: &[String]) -> Vec<(String, Option<String>)> {
    let mut result = Vec::new();
    let mut idx = 0;
    while idx < args.len() {
        if args[idx] == "--repo" {
            idx += 1;
            if idx < args.len() {
                let val = &args[idx];
                if let Some(colon_pos) = val.find(':') {
                    let alias = val[..colon_pos].to_string();
                    let branch = val[colon_pos + 1..].to_string();
                    result.push((alias, Some(branch)));
                } else {
                    result.push((val.clone(), None));
                }
            }
        }
        idx += 1;
    }
    result
}

fn extract_flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let mut idx = 0;
    while idx < args.len() {
        if args[idx] == flag {
            if idx + 1 < args.len() {
                return Some(&args[idx + 1]);
            }
        }
        idx += 1;
    }
    None
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// Extract the positional name (first arg that doesn't start with --)
fn extract_name(args: &[String]) -> Option<&str> {
    args.iter()
        .find(|a| !a.starts_with("--"))
        .map(|s| s.as_str())
}

fn ensure_worktrees_in_gitignore(meta_dir: &Path, worktrees_dirname: &str, quiet: bool) -> Result<()> {
    let gitignore_path = meta_dir.join(".gitignore");
    let pattern = format!("{}/", worktrees_dirname);

    if gitignore_path.exists() {
        let content = std::fs::read_to_string(&gitignore_path)?;
        if content
            .lines()
            .any(|line| line.trim() == pattern.trim_end_matches('/') || line.trim() == pattern)
        {
            return Ok(()); // already present
        }
        // Append
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&gitignore_path)?;
        writeln!(file, "{}", pattern)?;
    } else {
        std::fs::write(&gitignore_path, format!("{}\n", pattern))?;
    }
    if !quiet {
        eprintln!(
            "{} Added '{}' to .gitignore",
            "notice:".yellow().bold(),
            pattern
        );
    }
    Ok(())
}

// ==================== Discovery ====================

/// Discover repos within a worktree task directory by scanning for .git files.
fn discover_worktree_repos(task_dir: &Path) -> Result<Vec<WorktreeRepoInfo>> {
    let mut repos = Vec::new();

    // Check if the task dir itself is a worktree (the "." alias)
    let dot_git = task_dir.join(".git");
    if dot_git.exists() && dot_git.is_file() {
        let source = source_repo_from_gitfile(&dot_git)?;
        let branch = git_current_branch(task_dir).unwrap_or_else(|_| "HEAD".to_string());
        repos.push(WorktreeRepoInfo {
            alias: ".".to_string(),
            branch,
            path: task_dir.to_path_buf(),
            source_path: source,
            created_branch: None,
        });
    }

    // Scan subdirectories for worktrees
    if task_dir.is_dir() {
        for entry in std::fs::read_dir(task_dir)? {
            let entry = entry?;
            let sub_path = entry.path();
            if sub_path.is_dir() {
                let sub_git = sub_path.join(".git");
                if sub_git.exists() && sub_git.is_file() {
                    let source = source_repo_from_gitfile(&sub_git)?;
                    let branch =
                        git_current_branch(&sub_path).unwrap_or_else(|_| "HEAD".to_string());
                    let alias = sub_path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    repos.push(WorktreeRepoInfo {
                        alias,
                        branch,
                        path: sub_path,
                        source_path: source,
                        created_branch: None,
                    });
                }
            }
        }
    }

    Ok(repos)
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
        .ok_or_else(|| {
            anyhow::anyhow!("Cannot derive source repo from gitdir: {}", gitdir)
        })?;

    // dot_git_dir is now the .git directory; parent is the repo root
    let repo_root = dot_git_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot find repo root from .git dir: {}", dot_git_dir.display()))?;

    Ok(repo_root.to_path_buf())
}

// ==================== Git Operations ====================

fn git_worktree_add(repo_path: &Path, worktree_dest: &Path, branch: &str) -> Result<bool> {
    // Check if branch exists locally
    let branch_exists = Command::new("git")
        .args(["rev-parse", "--verify", &format!("refs/heads/{}", branch)])
        .current_dir(repo_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success();

    // Also check if branch exists on remote
    let remote_branch_exists = if !branch_exists {
        Command::new("git")
            .args(["rev-parse", "--verify", &format!("refs/remotes/origin/{}", branch)])
            .current_dir(repo_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success()
    } else {
        false
    };

    let output = if branch_exists {
        Command::new("git")
            .args([
                "worktree",
                "add",
                &worktree_dest.to_string_lossy(),
                branch,
            ])
            .current_dir(repo_path)
            .output()?
    } else if remote_branch_exists {
        // Create local tracking branch from remote
        Command::new("git")
            .args([
                "worktree",
                "add",
                "--track",
                "-b",
                branch,
                &worktree_dest.to_string_lossy(),
                &format!("origin/{}", branch),
            ])
            .current_dir(repo_path)
            .output()?
    } else {
        // Create new branch from HEAD
        Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                branch,
                &worktree_dest.to_string_lossy(),
            ])
            .current_dir(repo_path)
            .output()?
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "git worktree add failed for '{}' (branch: {}): {}",
            repo_path.display(),
            branch,
            stderr.trim()
        );
    }

    // Return whether we created a new branch
    let created_branch = !branch_exists && !remote_branch_exists;
    Ok(created_branch)
}

fn git_worktree_remove(repo_path: &Path, worktree_path: &Path, force: bool) -> Result<()> {
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    let wt_str = worktree_path.to_string_lossy();
    args.push(&wt_str);

    let output = Command::new("git")
        .args(&args)
        .current_dir(repo_path)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree remove failed: {}", stderr.trim());
    }
    Ok(())
}

fn git_is_dirty(repo_path: &Path) -> Result<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_path)
        .output()?;
    Ok(!output.stdout.is_empty())
}

fn git_current_branch(repo_path: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(repo_path)
        .output()?;
    if !output.status.success() {
        anyhow::bail!("Failed to get current branch");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_modified_files(repo_path: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["diff", "--name-only"])
        .current_dir(repo_path)
        .output()?;
    let files: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    // Also include staged changes
    let staged = Command::new("git")
        .args(["diff", "--name-only", "--cached"])
        .current_dir(repo_path)
        .output()?;
    let mut all_files = files;
    for line in String::from_utf8_lossy(&staged.stdout).lines() {
        if !line.is_empty() && !all_files.contains(&line.to_string()) {
            all_files.push(line.to_string());
        }
    }
    Ok(all_files)
}

fn git_untracked_count(repo_path: &Path) -> Result<usize> {
    let output = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(repo_path)
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .count())
}

fn git_ahead_behind(repo_path: &Path) -> Result<(u32, u32)> {
    let output = Command::new("git")
        .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .current_dir(repo_path)
        .stderr(Stdio::null())
        .output()?;

    if !output.status.success() {
        // No upstream configured
        return Ok((0, 0));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = text.trim().split('\t').collect();
    if parts.len() == 2 {
        let ahead = parts[0].parse::<u32>().unwrap_or(0);
        let behind = parts[1].parse::<u32>().unwrap_or(0);
        Ok((ahead, behind))
    } else {
        Ok((0, 0))
    }
}

fn git_diff_stat(worktree_path: &Path, base_ref: &str) -> Result<(usize, usize, usize, Vec<String>)> {
    // Try three-dot diff first (changes since divergence)
    let numstat_output = Command::new("git")
        .args(["diff", "--numstat", &format!("{}...HEAD", base_ref)])
        .current_dir(worktree_path)
        .stderr(Stdio::null())
        .output()?;

    let numstat_text = if numstat_output.status.success() {
        String::from_utf8_lossy(&numstat_output.stdout).to_string()
    } else {
        // Fallback to two-dot diff
        let fallback = Command::new("git")
            .args(["diff", "--numstat", &format!("{}..HEAD", base_ref)])
            .current_dir(worktree_path)
            .stderr(Stdio::null())
            .output()?;
        String::from_utf8_lossy(&fallback.stdout).to_string()
    };

    let mut files_changed = 0;
    let mut insertions = 0;
    let mut deletions = 0;
    let mut files = Vec::new();

    for line in numstat_text.lines() {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 3 {
            files_changed += 1;
            insertions += parts[0].parse::<usize>().unwrap_or(0);
            deletions += parts[1].parse::<usize>().unwrap_or(0);
            files.push(parts[2].to_string());
        }
    }

    Ok((files_changed, insertions, deletions, files))
}

// ==================== Subcommand: create ====================

fn handle_create(args: &[String], verbose: bool, json: bool) -> Result<()> {
    let name = extract_name(args)
        .ok_or_else(|| anyhow::anyhow!("Usage: meta worktree create <name> [--branch <branch>] [--repo <alias>[:<branch>]]... [--all] [--json]"))?;
    validate_worktree_name(name)?;

    let branch_flag = extract_flag_value(args, "--branch");
    let repo_specs = parse_repo_args(args);
    let use_all = has_flag(args, "--all");

    if repo_specs.is_empty() && !use_all {
        anyhow::bail!("Specify repos with --repo <alias> or use --all");
    }

    let meta_dir = find_meta_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not find .meta config. Run from within a meta repo."))?;
    let worktree_root = resolve_worktree_root(Some(&meta_dir))?;

    // Check if worktree already exists
    let wt_dir = worktree_root.join(name);
    if wt_dir.exists() {
        anyhow::bail!(
            "Worktree '{}' already exists at {}. Use 'meta worktree destroy {}' first.",
            name,
            wt_dir.display(),
            name
        );
    }

    // Parse .meta to get project list
    let (config_path, _) = config::find_meta_config(&meta_dir, None)
        .ok_or_else(|| anyhow::anyhow!("No .meta config found in {}", meta_dir.display()))?;
    let (projects, _) = config::parse_meta_config(&config_path)?;

    // Determine which repos to include: Vec<(alias, source_path, branch)>
    let repos_to_create: Vec<(String, PathBuf, String)> = if use_all {
        projects
            .iter()
            .map(|p| {
                let per_branch = repo_specs
                    .iter()
                    .find(|(a, _)| a == &p.name)
                    .and_then(|(_, b)| b.as_deref());
                (
                    p.name.clone(),
                    meta_dir.join(&p.path),
                    resolve_branch(name, branch_flag, per_branch),
                )
            })
            .collect()
    } else {
        let mut list = Vec::new();
        for (alias, per_branch) in &repo_specs {
            if alias == "." {
                list.push((
                    ".".to_string(),
                    meta_dir.clone(),
                    resolve_branch(name, branch_flag, per_branch.as_deref()),
                ));
            } else {
                let project = projects
                    .iter()
                    .find(|p| p.name == *alias)
                    .ok_or_else(|| {
                        let valid: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
                        anyhow::anyhow!(
                            "Unknown repo alias: '{}'. Valid aliases: {}",
                            alias,
                            valid.join(", ")
                        )
                    })?;
                list.push((
                    alias.clone(),
                    meta_dir.join(&project.path),
                    resolve_branch(name, branch_flag, per_branch.as_deref()),
                ));
            }
        }
        list
    };

    // Create worktree root dir
    std::fs::create_dir_all(&wt_dir)?;

    let dot_included = repos_to_create.iter().any(|(a, _, _)| a == ".");
    let mut created_repos = Vec::new();

    // If "." is included, create it first (it becomes the worktree root)
    if dot_included {
        let (_, source, branch) = repos_to_create
            .iter()
            .find(|(a, _, _)| a == ".")
            .unwrap();

        if verbose {
            eprintln!("Creating meta repo worktree at {} (branch: {})", wt_dir.display(), branch);
        }

        // Remove the empty dir we just created — git worktree add needs it to not exist
        std::fs::remove_dir(&wt_dir)?;

        let created_branch = git_worktree_add(source, &wt_dir, branch)?;
        created_repos.push(CreateRepoEntry {
            alias: ".".to_string(),
            path: wt_dir.display().to_string(),
            branch: branch.clone(),
            created_branch,
        });
    }

    // Create child repo worktrees
    for (alias, source, branch) in &repos_to_create {
        if alias == "." {
            continue;
        }

        let dest = wt_dir.join(alias);

        if verbose {
            eprintln!("Creating worktree for '{}' at {} (branch: {})", alias, dest.display(), branch);
        }

        let created_branch = git_worktree_add(source, &dest, branch)?;
        created_repos.push(CreateRepoEntry {
            alias: alias.clone(),
            path: dest.display().to_string(),
            branch: branch.clone(),
            created_branch,
        });
    }

    // Ensure .worktrees/ is in .gitignore
    let dirname = worktree_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(".worktrees");
    ensure_worktrees_in_gitignore(&meta_dir, dirname, json)?;

    // Output
    if json {
        let output = CreateOutput {
            name: name.to_string(),
            root: wt_dir.display().to_string(),
            repos: created_repos,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "{} Created worktree '{}' at {}",
            "✓".green(),
            name.bold(),
            wt_dir.display()
        );
        for r in &created_repos {
            let branch_note = if r.created_branch { " (new)" } else { "" };
            println!("  {} -> {}{}", r.alias, r.branch, branch_note);
        }
    }

    Ok(())
}

// ==================== Subcommand: add ====================

fn handle_add(args: &[String], verbose: bool, json: bool) -> Result<()> {
    let name = extract_name(args)
        .ok_or_else(|| anyhow::anyhow!("Usage: meta worktree add <name> --repo <alias>[:<branch>]"))?;
    validate_worktree_name(name)?;

    let repo_specs = parse_repo_args(args);
    if repo_specs.is_empty() {
        anyhow::bail!("--repo <alias>[:<branch>] is required for 'meta worktree add'");
    }

    // Check for "." alias
    if repo_specs.iter().any(|(a, _)| a == ".") {
        anyhow::bail!(
            "Cannot add '.' to an existing worktree. The meta repo root can only be established at create time.\n\
             Use 'meta worktree destroy {}' then 'meta worktree create {} --repo . ...' instead.",
            name, name
        );
    }

    let meta_dir = find_meta_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not find .meta config"))?;
    let worktree_root = resolve_worktree_root(Some(&meta_dir))?;
    let wt_dir = worktree_root.join(name);

    if !wt_dir.exists() {
        anyhow::bail!("Worktree '{}' not found at {}", name, wt_dir.display());
    }

    let (config_path, _) = config::find_meta_config(&meta_dir, None)
        .ok_or_else(|| anyhow::anyhow!("No .meta config found"))?;
    let (projects, _) = config::parse_meta_config(&config_path)?;

    // Check existing repos in the worktree
    let existing = discover_worktree_repos(&wt_dir)?;

    let mut added = Vec::new();
    for (alias, per_branch) in &repo_specs {
        if existing.iter().any(|r| r.alias == *alias) {
            anyhow::bail!("Repo '{}' already exists in worktree '{}'", alias, name);
        }

        let project = projects
            .iter()
            .find(|p| p.name == *alias)
            .ok_or_else(|| anyhow::anyhow!("Unknown repo alias: '{}'", alias))?;

        let source = meta_dir.join(&project.path);
        let branch = resolve_branch(name, None, per_branch.as_deref());
        let dest = wt_dir.join(alias);

        if verbose {
            eprintln!("Adding worktree for '{}' at {} (branch: {})", alias, dest.display(), branch);
        }

        let created_branch = git_worktree_add(&source, &dest, &branch)?;
        added.push(CreateRepoEntry {
            alias: alias.clone(),
            path: dest.display().to_string(),
            branch,
            created_branch,
        });
    }

    if json {
        let output = AddOutput {
            name: name.to_string(),
            repos: added,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        for r in &added {
            let branch_note = if r.created_branch { " (new)" } else { "" };
            println!(
                "{} Added '{}' to worktree '{}' (branch: {}{})",
                "✓".green(),
                r.alias,
                name,
                r.branch,
                branch_note
            );
        }
    }

    Ok(())
}

// ==================== Subcommand: list ====================

fn handle_list(_args: &[String], _verbose: bool, json: bool) -> Result<()> {
    let meta_dir = find_meta_dir();
    let worktree_root = resolve_worktree_root(meta_dir.as_deref())?;

    if !worktree_root.exists() {
        if json {
            println!("{}", serde_json::to_string_pretty(&ListOutput { worktrees: vec![] })?);
        } else {
            println!("No worktrees found.");
        }
        return Ok(());
    }

    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&worktree_root)? {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let task_dir = entry.path();

        let repos = discover_worktree_repos(&task_dir).unwrap_or_default();
        if repos.is_empty() {
            continue; // Not a valid worktree set
        }

        let has_meta_root = repos.iter().any(|r| r.alias == ".");
        let repo_entries: Vec<ListRepoEntry> = repos
            .iter()
            .map(|r| {
                let dirty = git_is_dirty(&r.path).unwrap_or(false);
                ListRepoEntry {
                    alias: r.alias.clone(),
                    branch: r.branch.clone(),
                    dirty,
                }
            })
            .collect();

        entries.push(ListEntry {
            name,
            root: task_dir.display().to_string(),
            has_meta_root,
            repos: repo_entries,
        });
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&ListOutput { worktrees: entries })?);
    } else if entries.is_empty() {
        println!("No worktrees found.");
    } else {
        for e in &entries {
            println!("{}", e.name.bold());
            for r in &e.repos {
                let status = if r.dirty {
                    "modified".yellow().to_string()
                } else {
                    "clean".green().to_string()
                };
                println!("  {:12} -> {:20} ({})", r.alias, r.branch, status);
            }
            println!();
        }
    }

    Ok(())
}

// ==================== Subcommand: status ====================

fn handle_status(args: &[String], _verbose: bool, json: bool) -> Result<()> {
    let name = extract_name(args)
        .ok_or_else(|| anyhow::anyhow!("Usage: meta worktree status <name> [--json]"))?;

    let meta_dir = find_meta_dir();
    let worktree_root = resolve_worktree_root(meta_dir.as_deref())?;
    let wt_dir = worktree_root.join(name);

    if !wt_dir.exists() {
        anyhow::bail!("Worktree '{}' not found at {}", name, wt_dir.display());
    }

    let repos = discover_worktree_repos(&wt_dir)?;
    if repos.is_empty() {
        anyhow::bail!("No repos found in worktree '{}'", name);
    }

    let mut statuses = Vec::new();
    for r in &repos {
        let dirty = git_is_dirty(&r.path).unwrap_or(false);
        let modified_files = if dirty {
            git_modified_files(&r.path).unwrap_or_default()
        } else {
            vec![]
        };
        let untracked = git_untracked_count(&r.path).unwrap_or(0);
        let (ahead, behind) = git_ahead_behind(&r.path).unwrap_or((0, 0));

        statuses.push(StatusRepoEntry {
            alias: r.alias.clone(),
            path: r.path.display().to_string(),
            branch: r.branch.clone(),
            dirty,
            modified_count: modified_files.len(),
            untracked_count: untracked,
            ahead,
            behind,
            modified_files,
        });
    }

    if json {
        let output = StatusOutput {
            name: name.to_string(),
            repos: statuses,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{}:", name.bold());
        for s in &statuses {
            let status_icon = if s.dirty {
                "●".yellow().to_string()
            } else {
                "✓".green().to_string()
            };
            let mut details = Vec::new();
            if s.modified_count > 0 {
                details.push(format!("{} modified", s.modified_count));
            }
            if s.untracked_count > 0 {
                details.push(format!("{} untracked", s.untracked_count));
            }
            if s.ahead > 0 {
                details.push(format!("↑{}", s.ahead));
            }
            if s.behind > 0 {
                details.push(format!("↓{}", s.behind));
            }
            let detail_str = if details.is_empty() {
                "clean".to_string()
            } else {
                details.join(", ")
            };
            println!(
                "  {} {:12} {:20} {}",
                status_icon, s.alias, s.branch, detail_str
            );
        }
    }

    Ok(())
}

// ==================== Subcommand: diff ====================

fn handle_diff(args: &[String], _verbose: bool, json: bool) -> Result<()> {
    let name = extract_name(args)
        .ok_or_else(|| anyhow::anyhow!("Usage: meta worktree diff <name> [--base <ref>] [--stat] [--json]"))?;
    let base_ref = extract_flag_value(args, "--base").unwrap_or("main");
    let stat_only = has_flag(args, "--stat");

    let meta_dir = find_meta_dir();
    let worktree_root = resolve_worktree_root(meta_dir.as_deref())?;
    let wt_dir = worktree_root.join(name);

    if !wt_dir.exists() {
        anyhow::bail!("Worktree '{}' not found at {}", name, wt_dir.display());
    }

    let repos = discover_worktree_repos(&wt_dir)?;
    if repos.is_empty() {
        anyhow::bail!("No repos found in worktree '{}'", name);
    }

    let mut diff_entries = Vec::new();
    let mut total_repos_changed = 0;
    let mut total_files = 0;
    let mut total_insertions = 0;
    let mut total_deletions = 0;

    for r in &repos {
        let (files_changed, insertions, deletions, files) =
            git_diff_stat(&r.path, base_ref).unwrap_or((0, 0, 0, vec![]));

        if files_changed > 0 {
            total_repos_changed += 1;
            total_files += files_changed;
            total_insertions += insertions;
            total_deletions += deletions;
        }

        diff_entries.push(DiffRepoEntry {
            alias: r.alias.clone(),
            base_ref: base_ref.to_string(),
            files_changed,
            insertions,
            deletions,
            files,
        });
    }

    if json {
        let output = DiffOutput {
            name: name.to_string(),
            base: base_ref.to_string(),
            repos: diff_entries,
            totals: DiffTotals {
                repos_changed: total_repos_changed,
                files_changed: total_files,
                insertions: total_insertions,
                deletions: total_deletions,
            },
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if stat_only || true {
        // Always show stat summary in human mode
        println!("{} vs {}:", name.bold(), base_ref);
        for d in &diff_entries {
            if d.files_changed > 0 {
                println!(
                    "  {:12} {} {} ({} files)",
                    d.alias,
                    format!("+{}", d.insertions).green(),
                    format!("-{}", d.deletions).red(),
                    d.files_changed,
                );
            }
        }
        if total_repos_changed > 0 {
            println!("  {}", "─".repeat(40));
            println!(
                "  {:12} {} {} ({} files, {} repos)",
                "Total",
                format!("+{}", total_insertions).green(),
                format!("-{}", total_deletions).red(),
                total_files,
                total_repos_changed,
            );
        } else {
            println!("  No changes vs {}", base_ref);
        }
    }

    Ok(())
}

// ==================== Subcommand: destroy ====================

fn handle_destroy(args: &[String], verbose: bool, _json: bool) -> Result<()> {
    let name = extract_name(args)
        .ok_or_else(|| anyhow::anyhow!("Usage: meta worktree destroy <name> [--force]"))?;
    let force = has_flag(args, "--force");

    let meta_dir = find_meta_dir();
    let worktree_root = resolve_worktree_root(meta_dir.as_deref())?;
    let wt_dir = worktree_root.join(name);

    if !wt_dir.exists() {
        anyhow::bail!("Worktree '{}' not found at {}", name, wt_dir.display());
    }

    let repos = discover_worktree_repos(&wt_dir)?;

    // Check for dirty repos (unless --force)
    if !force {
        let dirty_repos: Vec<&str> = repos
            .iter()
            .filter(|r| git_is_dirty(&r.path).unwrap_or(false))
            .map(|r| r.alias.as_str())
            .collect();

        if !dirty_repos.is_empty() {
            anyhow::bail!(
                "Worktree '{}' has uncommitted changes in: {}.\nUse --force to remove anyway.",
                name,
                dirty_repos.join(", ")
            );
        }
    }

    // Remove in reverse order: child repos first, then "." if present
    let child_repos: Vec<&WorktreeRepoInfo> =
        repos.iter().filter(|r| r.alias != ".").collect();
    let dot_repo = repos.iter().find(|r| r.alias == ".");

    // Remove child worktrees
    for r in &child_repos {
        if verbose {
            eprintln!("Removing worktree for '{}' at {}", r.alias, r.path.display());
        }
        match git_worktree_remove(&r.source_path, &r.path, force) {
            Ok(()) => {}
            Err(e) => {
                if force {
                    eprintln!(
                        "{} Failed to remove worktree for '{}': {}",
                        "warning:".yellow().bold(),
                        r.alias,
                        e
                    );
                } else {
                    return Err(e);
                }
            }
        }
    }

    // Remove "." worktree (must be last since children are inside it)
    if let Some(r) = dot_repo {
        if verbose {
            eprintln!("Removing meta repo worktree at {}", r.path.display());
        }
        match git_worktree_remove(&r.source_path, &r.path, force) {
            Ok(()) => {}
            Err(e) => {
                if force {
                    eprintln!(
                        "{} Failed to remove meta repo worktree: {}",
                        "warning:".yellow().bold(),
                        e
                    );
                } else {
                    return Err(e);
                }
            }
        }
    }

    // Clean up directory if it still exists
    if wt_dir.exists() {
        std::fs::remove_dir_all(&wt_dir).ok();
    }

    println!("{} Destroyed worktree '{}'", "✓".green(), name.bold());
    Ok(())
}

// ==================== Subcommand: exec ====================

fn handle_exec(args: &[String], verbose: bool, json: bool) -> Result<()> {
    // Parse: <name> [--include <a>] [--exclude <a>] [--parallel] [--json] -- <cmd>
    let name = extract_name(args)
        .ok_or_else(|| anyhow::anyhow!("Usage: meta worktree exec <name> [--include <a>] [--exclude <a>] [--parallel] -- <cmd>"))?;

    let meta_dir = find_meta_dir();
    let worktree_root = resolve_worktree_root(meta_dir.as_deref())?;
    let wt_dir = worktree_root.join(name);

    if !wt_dir.exists() {
        anyhow::bail!("Worktree '{}' not found at {}", name, wt_dir.display());
    }

    // Parse flags and extract command after "--"
    let mut include_filters: Vec<String> = Vec::new();
    let mut exclude_filters: Vec<String> = Vec::new();
    let mut parallel = false;
    let mut cmd_parts: Vec<String> = Vec::new();
    let mut past_separator = false;
    let mut json_flag = json;

    let mut idx = 0;
    while idx < args.len() {
        if args[idx] == "--" {
            past_separator = true;
            idx += 1;
            continue;
        }
        if past_separator {
            cmd_parts.push(args[idx].clone());
            idx += 1;
            continue;
        }
        match args[idx].as_str() {
            "--include" => {
                idx += 1;
                if idx < args.len() {
                    include_filters.extend(
                        args[idx].split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
                    );
                }
            }
            "--exclude" => {
                idx += 1;
                if idx < args.len() {
                    exclude_filters.extend(
                        args[idx].split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
                    );
                }
            }
            "--parallel" => parallel = true,
            "--json" => json_flag = true,
            _ => {} // name or unknown, skip
        }
        idx += 1;
    }

    if cmd_parts.is_empty() {
        anyhow::bail!("No command specified. Usage: meta worktree exec <name> -- <cmd>");
    }

    // Discover repos in the worktree
    let repos = discover_worktree_repos(&wt_dir)?;
    if repos.is_empty() {
        anyhow::bail!("No repos found in worktree '{}'", name);
    }

    // Build directories list
    let directories: Vec<String> = repos
        .iter()
        .map(|r| r.path.display().to_string())
        .collect();

    let command_str = cmd_parts.join(" ");

    let config = loop_lib::LoopConfig {
        directories,
        ignore: vec![],
        include_filters: if include_filters.is_empty() {
            None
        } else {
            Some(include_filters)
        },
        exclude_filters: if exclude_filters.is_empty() {
            None
        } else {
            Some(exclude_filters)
        },
        verbose,
        silent: false,
        parallel,
        dry_run: false,
        json_output: json_flag,
        add_aliases_to_global_looprc: false,
        spawn_stagger_ms: 0,
    };

    loop_lib::run(&config, &command_str)?;
    Ok(())
}
