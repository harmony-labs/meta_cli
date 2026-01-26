//! Multi-repo worktree management for meta.
//!
//! Provides `meta worktree` subcommands for creating, managing, and executing
//! commands across isolated git worktree sets.

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Args, Subcommand};
use colored::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use meta_cli::config;

// ==================== CLI Structs ====================

/// Worktree subcommands parsed by clap.
#[derive(Subcommand)]
pub enum WorktreeCommands {
    /// Create a new worktree set
    Create(CreateArgs),
    /// Add a repo to an existing worktree set
    Add(AddArgs),
    /// Remove a worktree set
    Destroy(DestroyArgs),
    /// List all worktree sets
    List(ListArgs),
    /// Show detailed status of a worktree set
    Status(StatusArgs),
    /// Show cross-repo diff vs base branch
    Diff(DiffArgs),
    /// Run a command across worktree repos
    Exec(ExecArgs),
    /// Remove expired/orphaned worktrees
    Prune(PruneArgs),
}

/// A repo specifier: `alias` or `alias:branch`
#[derive(Debug, Clone)]
pub struct RepoSpec {
    pub alias: String,
    pub branch: Option<String>,
}

impl std::str::FromStr for RepoSpec {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        if let Some(colon_pos) = s.find(':') {
            Ok(RepoSpec {
                alias: s[..colon_pos].to_string(),
                branch: Some(s[colon_pos + 1..].to_string()),
            })
        } else {
            Ok(RepoSpec {
                alias: s.to_string(),
                branch: None,
            })
        }
    }
}

/// A metadata key=value pair: `key=value`
#[derive(Debug, Clone)]
pub struct MetaKV {
    pub key: String,
    pub value: String,
}

impl std::str::FromStr for MetaKV {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let eq_pos = s.find('=').ok_or_else(|| {
            anyhow::anyhow!("--meta value '{s}' missing '=' separator (expected key=value)")
        })?;
        Ok(MetaKV {
            key: s[..eq_pos].to_string(),
            value: s[eq_pos + 1..].to_string(),
        })
    }
}

#[derive(Args)]
pub struct CreateArgs {
    /// Worktree name
    pub name: String,

    /// Override default branch name
    #[arg(long)]
    pub branch: Option<String>,

    /// Add specific repo(s) (alias or alias:branch)
    #[arg(long = "repo", value_name = "ALIAS[:BRANCH]")]
    pub repos: Vec<RepoSpec>,

    /// Add all repos from .meta config
    #[arg(long, conflicts_with = "repos")]
    pub all: bool,

    /// Start from a specific tag/SHA
    #[arg(long, value_name = "REF", conflicts_with = "from_pr")]
    pub from_ref: Option<String>,

    /// Start from a PR's head branch (owner/repo#N)
    #[arg(long, value_name = "OWNER/REPO#N", conflicts_with = "from_ref")]
    pub from_pr: Option<String>,

    /// Mark for automatic cleanup
    #[arg(long)]
    pub ephemeral: bool,

    /// Time-to-live (30s, 5m, 1h, 2d, 1w)
    #[arg(long, value_name = "DURATION", value_parser = parse_duration_clap)]
    pub ttl: Option<u64>,

    /// Store custom metadata (key=value)
    #[arg(long = "meta", value_name = "KEY=VALUE")]
    pub custom_meta: Vec<MetaKV>,
}

#[derive(Args)]
pub struct AddArgs {
    /// Worktree name
    pub name: String,

    /// Repo(s) to add (alias or alias:branch)
    #[arg(long = "repo", value_name = "ALIAS[:BRANCH]", required = true)]
    pub repos: Vec<RepoSpec>,
}

#[derive(Args)]
pub struct DestroyArgs {
    /// Worktree name
    pub name: String,

    /// Remove even with uncommitted changes
    #[arg(long)]
    pub force: bool,
}

#[derive(Args)]
pub struct ListArgs {}

#[derive(Args)]
pub struct StatusArgs {
    /// Worktree name
    pub name: String,
}

#[derive(Args)]
pub struct DiffArgs {
    /// Worktree name
    pub name: String,

    /// Base branch for comparison
    #[arg(long, default_value = "main")]
    pub base: String,
}

#[derive(Args)]
pub struct ExecArgs {
    /// Worktree name
    pub name: String,

    /// Only run in specified repos (comma-separated)
    #[arg(long, value_delimiter = ',')]
    pub include: Vec<String>,

    /// Skip specified repos (comma-separated)
    #[arg(long, value_delimiter = ',')]
    pub exclude: Vec<String>,

    /// Run commands in parallel
    #[arg(long)]
    pub parallel: bool,

    /// Atomic create+exec+destroy (requires --all or --repo, and -- <cmd>)
    #[arg(long)]
    pub ephemeral: bool,

    // --- Ephemeral-only create flags (ignored when not --ephemeral) ---
    /// Add specific repo(s) for ephemeral worktree (alias or alias:branch)
    #[arg(long = "repo", value_name = "ALIAS[:BRANCH]")]
    pub repos: Vec<RepoSpec>,

    /// Add all repos for ephemeral worktree
    #[arg(long)]
    pub all: bool,

    /// Store custom metadata for ephemeral worktree (key=value)
    #[arg(long = "meta", value_name = "KEY=VALUE")]
    pub custom_meta: Vec<MetaKV>,

    /// Start from a specific tag/SHA (ephemeral only)
    #[arg(long, value_name = "REF", conflicts_with = "from_pr")]
    pub from_ref: Option<String>,

    /// Start from a PR's head branch (ephemeral only, owner/repo#N)
    #[arg(long, value_name = "OWNER/REPO#N", conflicts_with = "from_ref")]
    pub from_pr: Option<String>,

    /// Override branch name for ephemeral worktree
    #[arg(long = "branch")]
    pub branch: Option<String>,

    /// Command and arguments to execute (after --)
    #[arg(last = true, required = true)]
    pub command: Vec<String>,
}

#[derive(Args)]
pub struct PruneArgs {
    /// Preview without removing
    #[arg(long)]
    pub dry_run: bool,
}

/// Parse a human-friendly duration string for clap value_parser.
fn parse_duration_clap(s: &str) -> std::result::Result<u64, String> {
    parse_duration(s).map_err(|e| e.to_string())
}

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

// ==================== Centralized Store Types ====================

/// Top-level store structure at `~/.meta/worktree.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct WorktreeStoreData {
    worktrees: HashMap<String, WorktreeStoreEntry>,
}

/// Individual worktree entry in the centralized store.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorktreeStoreEntry {
    name: String,
    project: String,
    created_at: String,
    ephemeral: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl_seconds: Option<u64>,
    repos: Vec<StoreRepoEntry>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    custom: HashMap<String, String>,
}

/// Repo entry within a store entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoreRepoEntry {
    alias: String,
    branch: String,
    created_branch: bool,
}

// ==================== JSON Output Structures ====================

#[derive(Debug, Serialize)]
struct CreateOutput {
    name: String,
    root: String,
    repos: Vec<CreateRepoEntry>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    ephemeral: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl_seconds: Option<u64>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    custom: HashMap<String, String>,
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
struct DestroyOutput {
    name: String,
    path: String,
    repos_removed: usize,
}

#[derive(Debug, Serialize)]
struct ListEntry {
    name: String,
    root: String,
    has_meta_root: bool,
    repos: Vec<ListRepoEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ephemeral: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl_remaining_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom: Option<HashMap<String, String>>,
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

// ==================== Context Detection ====================

/// Detect if cwd is inside a `.worktrees/<name>/` directory.
/// Returns (task_name, task_dir, repo_paths) if inside a worktree, None otherwise.
/// Filesystem-based detection — no store dependency.
///
/// Walks the path components looking for a `.worktrees` segment followed by a task name.
/// Works whether cwd is the task dir itself, a repo within it, or deeper inside a repo.
pub fn detect_worktree_context(cwd: &std::path::Path) -> Option<(String, PathBuf, Vec<PathBuf>)> {
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

// ==================== Entry Point ====================

pub fn handle_worktree_command(command: WorktreeCommands, verbose: bool, json: bool) -> Result<()> {
    match command {
        WorktreeCommands::Create(args) => handle_create(args, verbose, json),
        WorktreeCommands::Add(args) => handle_add(args, verbose, json),
        WorktreeCommands::Destroy(args) => handle_destroy(args, verbose, json),
        WorktreeCommands::List(args) => handle_list(args, verbose, json),
        WorktreeCommands::Status(args) => handle_status(args, verbose, json),
        WorktreeCommands::Diff(args) => handle_diff(args, verbose, json),
        WorktreeCommands::Exec(args) => handle_exec(args, verbose, json),
        WorktreeCommands::Prune(args) => handle_prune(args, verbose, json),
    }
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
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!(
            "Invalid worktree name '{}': only ASCII alphanumeric characters, hyphens, and underscores allowed",
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

/// Read and parse the .meta config as a JSON Value.
/// Tries .meta, .meta.yaml, .meta.yml in order, parsing JSON or YAML as appropriate.
fn read_meta_config_value(meta_dir: &Path) -> Option<serde_json::Value> {
    for name in &[".meta", ".meta.yaml", ".meta.yml"] {
        let path = meta_dir.join(name);
        if !path.exists() {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // Try JSON first
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
            return Some(v);
        }
        // Try YAML
        if let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
            // Convert YAML Value to JSON Value for uniform access
            if let Ok(json_str) = serde_json::to_string(&v) {
                if let Ok(json_val) = serde_json::from_str(&json_str) {
                    return Some(json_val);
                }
            }
        }
    }
    None
}

fn read_worktrees_dir_from_config(meta_dir: &Path) -> Option<String> {
    read_meta_config_value(meta_dir)?
        .get("worktrees_dir")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn find_meta_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    config::find_meta_config(&cwd, None)
        .map(|(path, _)| path.parent().unwrap_or(Path::new(".")).to_path_buf())
}

/// Resolved worktree context for operations that need meta_dir, worktree_root, and worktree path.
#[allow(dead_code)] // meta_dir/worktree_root available for hooks and future expansion
struct WorktreeContext {
    meta_dir: Option<PathBuf>,
    worktree_root: PathBuf,
    wt_dir: PathBuf,
}

/// Resolve worktree context for a named worktree.
/// Returns meta_dir (for hooks), worktree_root, and the specific worktree directory.
fn resolve_worktree_context(name: &str) -> Result<WorktreeContext> {
    let meta_dir = find_meta_dir();
    let worktree_root = resolve_worktree_root(meta_dir.as_deref())?;
    let wt_dir = worktree_root.join(name);
    Ok(WorktreeContext { meta_dir, worktree_root, wt_dir })
}

/// Resolve worktree context and verify the worktree exists.
fn resolve_existing_worktree(name: &str) -> Result<WorktreeContext> {
    let ctx = resolve_worktree_context(name)?;
    if !ctx.wt_dir.exists() {
        anyhow::bail!("Worktree '{}' not found at {}", name, ctx.wt_dir.display());
    }
    Ok(ctx)
}

fn resolve_branch(task_name: &str, branch_flag: Option<&str>, per_repo_branch: Option<&str>) -> String {
    per_repo_branch
        .or(branch_flag)
        .map(|s| s.to_string())
        .unwrap_or_else(|| task_name.to_string())
}

/// Parse a human-friendly duration string to seconds.
/// Supported formats: "30s", "5m", "1h", "2d", "1w"
fn parse_duration(s: &str) -> Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("Empty duration string");
    }

    let (num_str, suffix) = s.split_at(s.len() - 1);
    let num: u64 = num_str
        .parse()
        .with_context(|| format!("Invalid duration number: '{num_str}'"))?;

    let multiplier = match suffix {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        "w" => 604800,
        _ => anyhow::bail!(
            "Invalid duration suffix '{}'. Use s (seconds), m (minutes), h (hours), d (days), or w (weeks)",
            suffix
        ),
    };

    Ok(num * multiplier)
}

/// Format seconds into a human-friendly duration string.
/// Returns the largest appropriate unit (e.g., "2h" not "7200s").
fn format_duration(secs: i64) -> String {
    let abs_secs = secs.unsigned_abs();
    let prefix = if secs < 0 { "-" } else { "" };

    if abs_secs >= 604800 && abs_secs.is_multiple_of(604800) {
        let weeks = abs_secs / 604800;
        format!("{prefix}{weeks}w")
    } else if abs_secs >= 86400 && abs_secs.is_multiple_of(86400) {
        let days = abs_secs / 86400;
        format!("{prefix}{days}d")
    } else if abs_secs >= 3600 && abs_secs.is_multiple_of(3600) {
        let hours = abs_secs / 3600;
        format!("{prefix}{hours}h")
    } else if abs_secs >= 60 && abs_secs.is_multiple_of(60) {
        let mins = abs_secs / 60;
        format!("{prefix}{mins}m")
    } else {
        format!("{prefix}{abs_secs}s")
    }
}

/// Parse `--from-pr owner/repo#N` format and resolve the PR's head branch.
/// Returns (owner/repo, pr_number, head_branch_name).
fn resolve_from_pr(from_pr: &str) -> Result<(String, u32, String)> {
    // Parse format: owner/repo#N
    let hash_pos = from_pr
        .rfind('#')
        .ok_or_else(|| anyhow::anyhow!("Invalid --from-pr format: '{from_pr}'. Expected: owner/repo#N"))?;

    let repo_spec = &from_pr[..hash_pos];
    // Validate repo spec format: must be owner/repo
    if !repo_spec.contains('/') || repo_spec.starts_with('/') || repo_spec.ends_with('/') {
        anyhow::bail!(
            "Invalid repo spec '{}' in --from-pr. Expected: owner/repo#N",
            repo_spec
        );
    }
    let pr_num: u32 = from_pr[hash_pos + 1..]
        .parse()
        .with_context(|| format!("Invalid PR number in '{from_pr}'"))?;

    // Resolve head branch via gh CLI
    let output = Command::new("gh")
        .args([
            "pr", "view",
            &pr_num.to_string(),
            "--repo", repo_spec,
            "--json", "headRefName",
            "-q", ".headRefName",
        ])
        .output()
        .with_context(|| "Failed to run 'gh' CLI. Is it installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Failed to resolve PR #{} in {}: {}",
            pr_num, repo_spec, stderr.trim()
        );
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        anyhow::bail!("Empty head branch for PR #{} in {}", pr_num, repo_spec);
    }

    Ok((repo_spec.to_string(), pr_num, branch))
}

/// Check if a repo's remote URL matches the given owner/repo spec.
fn repo_matches_spec(repo_path: &Path, spec: &str) -> bool {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(repo_path)
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let url = String::from_utf8_lossy(&o.stdout).trim().to_string();
            // Match against github.com:owner/repo or github.com/owner/repo
            url.contains(spec) || url.contains(&spec.replace('/', ":"))
        }
        _ => false,
    }
}

/// Fetch a branch from origin if not locally available.
fn git_fetch_branch(repo_path: &Path, branch: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["fetch", "origin", branch])
        .current_dir(repo_path)
        .stderr(Stdio::piped())
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to fetch branch '{}': {}", branch, stderr.trim());
    }
    Ok(())
}

fn ensure_worktrees_in_gitignore(meta_dir: &Path, worktrees_dirname: &str, quiet: bool) -> Result<()> {
    let gitignore_path = meta_dir.join(".gitignore");
    let pattern = format!("{worktrees_dirname}/");

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
        writeln!(file, "{pattern}")?;
    } else {
        std::fs::write(&gitignore_path, format!("{pattern}\n"))?;
    }
    if !quiet {
        eprintln!(
            "{} Added '{pattern}' to .gitignore",
            "notice:".yellow().bold(),
        );
    }
    Ok(())
}

// ==================== Store Operations ====================

fn store_path() -> PathBuf {
    meta_core::data_dir::data_file("worktree")
}

fn store_lock_path(data_path: &Path) -> PathBuf {
    data_path.with_extension("lock")
}

/// Add a worktree entry to the centralized store.
fn store_add(worktree_path: &Path, entry: WorktreeStoreEntry) -> Result<()> {
    meta_core::data_dir::ensure_meta_dir()?;
    let data_path = store_path();
    let lock_path = store_lock_path(&data_path);
    let key = worktree_path.to_string_lossy().to_string();

    meta_core::store::update::<WorktreeStoreData, _>(&data_path, &lock_path, |store| {
        store.worktrees.insert(key, entry);
    })
}

/// Remove a worktree entry from the centralized store.
fn store_remove(worktree_path: &Path) -> Result<()> {
    let data_path = store_path();
    if !data_path.exists() {
        return Ok(());
    }
    let lock_path = store_lock_path(&data_path);
    let key = worktree_path.to_string_lossy().to_string();

    meta_core::store::update::<WorktreeStoreData, _>(&data_path, &lock_path, |store| {
        store.worktrees.remove(&key);
    })
}

/// Get all entries from the store.
fn store_list() -> Result<WorktreeStoreData> {
    meta_core::store::read(&store_path())
}

/// Compute TTL remaining seconds for a store entry.
/// Returns `None` if no TTL is set. Negative means expired.
fn entry_ttl_remaining(entry: &WorktreeStoreEntry, now_epoch: i64) -> Option<i64> {
    entry.ttl_seconds.map(|ttl| {
        let created = chrono::DateTime::parse_from_rfc3339(&entry.created_at)
            .map(|dt| dt.timestamp())
            .unwrap_or(0);
        created + ttl as i64 - now_epoch
    })
}

// ==================== Lifecycle Hooks ====================

/// Fire a worktree lifecycle hook if configured in `.meta`.
///
/// Reads the `.meta` config for `worktree.hooks.<hook_name>`.
/// If configured, spawns the command and pipes `payload` JSON to stdin.
/// Hook failure prints a warning but doesn't block the operation.
fn fire_worktree_hook(hook_name: &str, payload: &serde_json::Value, meta_dir: Option<&Path>) {
    let dir = match meta_dir {
        Some(d) => d,
        None => return,
    };

    let config = match read_meta_config_value(dir) {
        Some(c) => c,
        None => return,
    };

    let hook_cmd = config
        .get("worktree")
        .and_then(|wt| wt.get("hooks"))
        .and_then(|hooks| hooks.get(hook_name))
        .and_then(|v| v.as_str());

    let cmd_str = match hook_cmd {
        Some(c) => c,
        None => return,
    };

    let payload_json = match serde_json::to_string(payload) {
        Ok(j) => j,
        Err(_) => return,
    };

    let result = Command::new("sh")
        .args(["-c", cmd_str])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            // Write payload then drop stdin to signal EOF before waiting
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(payload_json.as_bytes());
            }
            // stdin is now dropped — child sees EOF
            child.wait()
        });

    match result {
        Ok(status) if !status.success() => {
            eprintln!(
                "{} Hook '{}' exited with status {}",
                "warning:".yellow().bold(),
                hook_name,
                status
            );
        }
        Err(e) => {
            eprintln!(
                "{} Hook '{}' failed to execute: {}",
                "warning:".yellow().bold(),
                hook_name,
                e
            );
        }
        _ => {}
    }
}

// ==================== Discovery ====================

/// Discover repos within a worktree task directory by scanning for .git files.
/// Results are sorted by alias for deterministic output.
fn discover_worktree_repos(task_dir: &Path) -> Result<Vec<WorktreeRepoInfo>> {
    let mut repos = Vec::new();

    // Check if the task dir itself is a worktree (the "." alias)
    let dot_git = task_dir.join(".git");
    if dot_git.symlink_metadata().map(|m| m.is_file()).unwrap_or(false) {
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
            if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                continue;
            }
            let sub_git = sub_path.join(".git");
            if sub_git.symlink_metadata().map(|m| m.is_file()).unwrap_or(false) {
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

    // Sort by alias for deterministic output ("." sorts first)
    repos.sort_by(|a, b| a.alias.cmp(&b.alias));

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

fn git_worktree_add(repo_path: &Path, worktree_dest: &Path, branch: &str, from_ref: Option<&str>) -> Result<bool> {
    // If from_ref is specified, verify it exists in this repo
    if let Some(ref_name) = from_ref {
        let ref_exists = Command::new("git")
            .args(["rev-parse", "--verify", ref_name])
            .current_dir(repo_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success();

        if !ref_exists {
            anyhow::bail!(
                "Ref '{}' not found in repo '{}'",
                ref_name,
                repo_path.display()
            );
        }

        // Create branch from the specified ref
        let output = Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                branch,
                &worktree_dest.to_string_lossy(),
                ref_name,
            ])
            .current_dir(repo_path)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "git worktree add failed for '{}' (branch: {}, ref: {}): {}",
                repo_path.display(),
                branch,
                ref_name,
                stderr.trim()
            );
        }
        return Ok(true); // Always creates a new branch from ref
    }

    // Check if branch exists locally
    let branch_exists = Command::new("git")
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .current_dir(repo_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success();

    // Also check if branch exists on remote
    let remote_branch_exists = if !branch_exists {
        Command::new("git")
            .args(["rev-parse", "--verify", &format!("refs/remotes/origin/{branch}")])
            .current_dir(repo_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success()
    } else {
        false
    };

    let dest_str = worktree_dest.to_string_lossy();
    let remote_ref = format!("origin/{branch}");

    let wt_args: Vec<&str> = if branch_exists {
        // Use existing local branch
        vec!["worktree", "add", &dest_str, branch]
    } else if remote_branch_exists {
        // Create local tracking branch from remote
        vec!["worktree", "add", "--track", "-b", branch, &dest_str, &remote_ref]
    } else {
        // Create new branch from HEAD
        vec!["worktree", "add", "-b", branch, &dest_str]
    };

    let output = Command::new("git")
        .args(&wt_args)
        .current_dir(repo_path)
        .output()?;

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

/// Combined git status summary from a single `git status --porcelain` call.
/// Returns dirty state, modified file list, and untracked count in one subprocess call.
struct GitStatusSummary {
    dirty: bool,
    modified_files: Vec<String>,
    untracked_count: usize,
}

fn git_status_summary(repo_path: &Path) -> Result<GitStatusSummary> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_path)
        .output()?;

    let mut modified_files = Vec::new();
    let mut untracked_count = 0;

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.len() < 3 {
            continue;
        }
        let status = &line[..2];
        let file = &line[3..];

        if status == "??" {
            untracked_count += 1;
        } else if !file.is_empty() {
            // Tracked file with modifications (staged, unstaged, or both).
            // For renames ("R  old -> new"), extract the new name.
            let name = file.split(" -> ").last().unwrap_or(file);
            modified_files.push(name.to_string());
        }
    }

    let dirty = !modified_files.is_empty() || untracked_count > 0;
    Ok(GitStatusSummary {
        dirty,
        modified_files,
        untracked_count,
    })
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
        .args(["diff", "--numstat", &format!("{base_ref}...HEAD")])
        .current_dir(worktree_path)
        .stderr(Stdio::null())
        .output()?;

    let numstat_text = if numstat_output.status.success() {
        String::from_utf8_lossy(&numstat_output.stdout).to_string()
    } else {
        // Fallback to two-dot diff
        let fallback = Command::new("git")
            .args(["diff", "--numstat", &format!("{base_ref}..HEAD")])
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

fn handle_create(args: CreateArgs, verbose: bool, json: bool) -> Result<()> {
    let name = &args.name;
    validate_worktree_name(name)?;

    let branch_flag = args.branch.as_deref();
    let repo_specs: Vec<(String, Option<String>)> = args.repos.iter()
        .map(|r| (r.alias.clone(), r.branch.clone()))
        .collect();
    let use_all = args.all;
    let ephemeral = args.ephemeral;
    let ttl_seconds = args.ttl;
    let custom_meta: HashMap<String, String> = args.custom_meta.iter()
        .map(|kv| (kv.key.clone(), kv.value.clone()))
        .collect();
    let from_ref = args.from_ref.as_deref();
    let from_pr_spec = args.from_pr.as_deref();

    // Resolve --from-pr: get PR head branch and identify matching repo
    let from_pr_info = from_pr_spec
        .map(resolve_from_pr)
        .transpose()?;

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

    // Build project lookup for O(1) access by alias
    let project_map: HashMap<&str, &config::ProjectInfo> = projects
        .iter()
        .map(|p| (p.name.as_str(), p))
        .collect();

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
                let project = project_map.get(alias.as_str()).ok_or_else(|| {
                    let valid: Vec<&str> = project_map.keys().copied().collect();
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

    // Apply --from-pr: override branch for the matching repo and fetch
    let mut repos_to_create = repos_to_create;
    if let Some((ref pr_repo_spec, _pr_num, ref pr_branch)) = from_pr_info {
        let mut matched = false;
        for (alias, source, branch) in repos_to_create.iter_mut() {
            if *alias != "." && repo_matches_spec(source, pr_repo_spec) {
                // Fetch the PR branch
                if let Err(e) = git_fetch_branch(source, pr_branch) {
                    eprintln!("{} Failed to fetch PR branch '{}': {}", "warning:".yellow().bold(), pr_branch, e);
                }
                *branch = pr_branch.clone();
                matched = true;
                break;
            }
        }
        if !matched {
            eprintln!(
                "{} No repo matches '{}'. PR branch '{}' not applied.",
                "warning:".yellow().bold(),
                pr_repo_spec,
                pr_branch
            );
        }
    }

    let dot_included = repos_to_create.iter().any(|(a, _, _)| a == ".");
    let mut created_repos = Vec::new();

    // If "." is included, create it first (it becomes the worktree root).
    // git worktree add creates the target dir, so we skip create_dir_all.
    if dot_included {
        let (_, source, branch) = repos_to_create
            .iter()
            .find(|(a, _, _)| a == ".")
            .unwrap();

        if verbose {
            eprintln!("Creating meta repo worktree at {} (branch: {})", wt_dir.display(), branch);
        }

        // Ensure parent exists (git worktree add creates the leaf dir)
        if let Some(parent) = wt_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let created_branch = git_worktree_add(source, &wt_dir, branch, from_ref)?;
        created_repos.push(CreateRepoEntry {
            alias: ".".to_string(),
            path: wt_dir.display().to_string(),
            branch: branch.clone(),
            created_branch,
        });
    }

    // Ensure wt_dir exists for child repos (when "." isn't included, it wasn't created by git)
    if !dot_included {
        std::fs::create_dir_all(&wt_dir)?;
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

        match git_worktree_add(source, &dest, branch, from_ref) {
            Ok(created_branch) => {
                created_repos.push(CreateRepoEntry {
                    alias: alias.clone(),
                    path: dest.display().to_string(),
                    branch: branch.clone(),
                    created_branch,
                });
            }
            Err(e) if from_ref.is_some() => {
                // --from-ref: skip repos where ref doesn't exist
                eprintln!(
                    "{} Skipping '{}': {}",
                    "warning:".yellow().bold(),
                    alias,
                    e
                );
                continue;
            }
            Err(e) => return Err(e),
        }
    }

    // Ensure .worktrees/ is in .gitignore
    let dirname = worktree_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(".worktrees");
    ensure_worktrees_in_gitignore(&meta_dir, dirname, json)?;

    // Add to centralized store
    let store_entry = WorktreeStoreEntry {
        name: name.to_string(),
        project: meta_dir.to_string_lossy().to_string(),
        created_at: Utc::now().to_rfc3339(),
        ephemeral,
        ttl_seconds,
        repos: created_repos
            .iter()
            .map(|r| StoreRepoEntry {
                alias: r.alias.clone(),
                branch: r.branch.clone(),
                created_branch: r.created_branch,
            })
            .collect(),
        custom: custom_meta.clone(),
    };
    if let Err(e) = store_add(&wt_dir, store_entry) {
        eprintln!("{} Failed to update store: {}", "warning:".yellow().bold(), e);
    }

    // Fire post-create hook
    let hook_payload = serde_json::json!({
        "action": "create",
        "name": name,
        "path": wt_dir.display().to_string(),
        "repos": created_repos.iter().map(|r| serde_json::json!({
            "alias": r.alias,
            "branch": r.branch,
            "created_branch": r.created_branch,
        })).collect::<Vec<_>>(),
        "ephemeral": ephemeral,
        "ttl_seconds": ttl_seconds,
        "custom": custom_meta,
    });
    fire_worktree_hook("post-create", &hook_payload, Some(&meta_dir));

    // Output
    if json {
        let output = CreateOutput {
            name: name.to_string(),
            root: wt_dir.display().to_string(),
            repos: created_repos,
            ephemeral,
            ttl_seconds,
            custom: custom_meta,
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
        if ephemeral {
            println!("  {}", "[ephemeral]".dimmed());
        }
        if let Some(ttl) = ttl_seconds {
            println!("  {}", format!("[TTL: {}]", format_duration(ttl as i64)).dimmed());
        }
    }

    Ok(())
}

// ==================== Subcommand: add ====================

fn handle_add(args: AddArgs, verbose: bool, json: bool) -> Result<()> {
    let name = &args.name;
    validate_worktree_name(name)?;

    let repo_specs: Vec<(String, Option<String>)> = args.repos.iter()
        .map(|r| (r.alias.clone(), r.branch.clone()))
        .collect();

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

    // Build project lookup for O(1) access by alias
    let project_map: HashMap<&str, &config::ProjectInfo> = projects
        .iter()
        .map(|p| (p.name.as_str(), p))
        .collect();

    let mut added = Vec::new();
    for (alias, per_branch) in &repo_specs {
        if existing.iter().any(|r| r.alias == *alias) {
            anyhow::bail!("Repo '{}' already exists in worktree '{}'", alias, name);
        }

        let project = project_map.get(alias.as_str()).ok_or_else(|| {
            let valid: Vec<&str> = project_map.keys().copied().collect();
            anyhow::anyhow!(
                "Unknown repo alias: '{}'. Valid aliases: {}",
                alias,
                valid.join(", ")
            )
        })?;

        let source = meta_dir.join(&project.path);
        let branch = resolve_branch(name, None, per_branch.as_deref());
        let dest = wt_dir.join(alias);

        if verbose {
            eprintln!("Adding worktree for '{}' at {} (branch: {})", alias, dest.display(), branch);
        }

        let created_branch = git_worktree_add(&source, &dest, &branch, None)?;
        added.push(CreateRepoEntry {
            alias: alias.clone(),
            path: dest.display().to_string(),
            branch,
            created_branch,
        });
    }

    // Update centralized store
    let data_path = store_path();
    let lock_path = store_lock_path(&data_path);
    let wt_key = wt_dir.to_string_lossy().to_string();
    let new_repos: Vec<StoreRepoEntry> = added
        .iter()
        .map(|r| StoreRepoEntry {
            alias: r.alias.clone(),
            branch: r.branch.clone(),
            created_branch: r.created_branch,
        })
        .collect();
    if let Err(e) = meta_core::store::update::<WorktreeStoreData, _>(&data_path, &lock_path, move |store| {
        if let Some(entry) = store.worktrees.get_mut(&wt_key) {
            entry.repos.extend(new_repos);
        }
    }) {
        eprintln!("{} Failed to update store: {}", "warning:".yellow().bold(), e);
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

fn handle_list(_args: ListArgs, _verbose: bool, json: bool) -> Result<()> {
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

    // Load store data for metadata enrichment
    let store_data = store_list().unwrap_or_default();
    let now = Utc::now().timestamp();

    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&worktree_root)? {
        let entry = entry?;
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
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
                let dirty = git_status_summary(&r.path)
                    .map(|s| s.dirty)
                    .unwrap_or(false);
                ListRepoEntry {
                    alias: r.alias.clone(),
                    branch: r.branch.clone(),
                    dirty,
                }
            })
            .collect();

        // Merge store metadata if available
        let task_key = task_dir.to_string_lossy().to_string();
        let (ephemeral, ttl_remaining, custom) =
            if let Some(store_entry) = store_data.worktrees.get(&task_key) {
                let custom = if store_entry.custom.is_empty() {
                    None
                } else {
                    Some(store_entry.custom.clone())
                };
                (Some(store_entry.ephemeral), entry_ttl_remaining(store_entry, now), custom)
            } else {
                (None, None, None)
            };

        entries.push(ListEntry {
            name,
            root: task_dir.display().to_string(),
            has_meta_root,
            repos: repo_entries,
            ephemeral,
            ttl_remaining_seconds: ttl_remaining,
            custom,
        });
    }

    // Sort by name for deterministic output
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    if json {
        println!("{}", serde_json::to_string_pretty(&ListOutput { worktrees: entries })?);
    } else if entries.is_empty() {
        println!("No worktrees found.");
    } else {
        for e in &entries {
            let mut header = e.name.bold().to_string();
            if e.ephemeral == Some(true) {
                header.push_str(&format!(" {}", "[ephemeral]".dimmed()));
            }
            if let Some(ttl) = e.ttl_remaining_seconds {
                if ttl > 0 {
                    header.push_str(&format!(" {}", format!("[TTL: {}]", format_duration(ttl)).dimmed()));
                } else {
                    header.push_str(&format!(" {}", "[expired]".red()));
                }
            }
            println!("{header}");
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

fn handle_status(args: StatusArgs, _verbose: bool, json: bool) -> Result<()> {
    let name = &args.name;

    let ctx = resolve_existing_worktree(name)?;

    let repos = discover_worktree_repos(&ctx.wt_dir)?;
    if repos.is_empty() {
        anyhow::bail!("No repos found in worktree '{}'", name);
    }

    let mut statuses = Vec::new();
    for r in &repos {
        let summary = git_status_summary(&r.path).unwrap_or(GitStatusSummary {
            dirty: false,
            modified_files: vec![],
            untracked_count: 0,
        });
        let (ahead, behind) = git_ahead_behind(&r.path).unwrap_or((0, 0));

        statuses.push(StatusRepoEntry {
            alias: r.alias.clone(),
            path: r.path.display().to_string(),
            branch: r.branch.clone(),
            dirty: summary.dirty,
            modified_count: summary.modified_files.len(),
            untracked_count: summary.untracked_count,
            ahead,
            behind,
            modified_files: summary.modified_files,
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

fn handle_diff(args: DiffArgs, _verbose: bool, json: bool) -> Result<()> {
    let name = &args.name;
    let base_ref = &args.base;

    let ctx = resolve_existing_worktree(name)?;

    let repos = discover_worktree_repos(&ctx.wt_dir)?;
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
    } else {
        // Human mode: always show stat summary
        println!("{} vs {}:", name.bold(), base_ref);
        for d in &diff_entries {
            if d.files_changed > 0 {
                let insertions = d.insertions;
                let deletions = d.deletions;
                println!(
                    "  {:12} {} {} ({} files)",
                    d.alias,
                    format!("+{insertions}").green(),
                    format!("-{deletions}").red(),
                    d.files_changed,
                );
            }
        }
        if total_repos_changed > 0 {
            println!("  {}", "─".repeat(40));
            println!(
                "  {:12} {} {} ({} files, {} repos)",
                "Total",
                format!("+{total_insertions}").green(),
                format!("-{total_deletions}").red(),
                total_files,
                total_repos_changed,
            );
        } else {
            println!("  No changes vs {base_ref}");
        }
    }

    Ok(())
}

// ==================== Subcommand: destroy ====================

fn handle_destroy(args: DestroyArgs, verbose: bool, json: bool) -> Result<()> {
    let name = &args.name;
    let force = args.force;

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
            .filter(|r| git_status_summary(&r.path).map(|s| s.dirty).unwrap_or(false))
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

    // Remove from centralized store
    if let Err(e) = store_remove(&wt_dir) {
        eprintln!("{} Failed to update store: {}", "warning:".yellow().bold(), e);
    }

    // Fire post-destroy hook
    let hook_payload = serde_json::json!({
        "action": "destroy",
        "name": name,
        "path": wt_dir.display().to_string(),
        "force": force,
    });
    fire_worktree_hook("post-destroy", &hook_payload, meta_dir.as_deref());

    if json {
        let output = DestroyOutput {
            name: name.to_string(),
            path: wt_dir.display().to_string(),
            repos_removed: repos.len(),
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{} Destroyed worktree '{}'", "✓".green(), name.bold());
    }
    Ok(())
}

// ==================== Subcommand: exec ====================

fn handle_exec(args: ExecArgs, verbose: bool, json: bool) -> Result<()> {
    if args.ephemeral {
        return handle_ephemeral_exec(args, verbose, json);
    }

    let name = &args.name;
    let ctx = resolve_existing_worktree(name)?;

    let include_filters = args.include;
    let exclude_filters = args.exclude;
    let cmd_parts = args.command;

    // Discover repos in the worktree
    let repos = discover_worktree_repos(&ctx.wt_dir)?;
    if repos.is_empty() {
        anyhow::bail!("No repos found in worktree '{name}'");
    }

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
        parallel: args.parallel,
        dry_run: false,
        json_output: json,
        add_aliases_to_global_looprc: false,
        spawn_stagger_ms: 0,
    };

    loop_lib::run(&config, &command_str)?;
    Ok(())
}

// ==================== Ephemeral Exec ====================

fn handle_ephemeral_exec(args: ExecArgs, verbose: bool, json: bool) -> Result<()> {
    let name = args.name.clone();
    validate_worktree_name(&name)?;

    let cmd_parts = args.command;
    if cmd_parts.is_empty() {
        anyhow::bail!("No command specified after --");
    }

    // Build CreateArgs from the exec args
    let create_args = CreateArgs {
        name: name.clone(),
        branch: args.branch,
        repos: args.repos,
        all: args.all,
        from_ref: args.from_ref,
        from_pr: args.from_pr,
        ephemeral: true,
        ttl: None,
        custom_meta: args.custom_meta,
    };

    if verbose {
        eprintln!("Creating ephemeral worktree '{name}'...");
    }
    handle_create(create_args, verbose, json)?;

    // Resolve worktree path for exec
    let meta_dir = find_meta_dir();
    let worktree_root = resolve_worktree_root(meta_dir.as_deref())?;
    let wt_dir = worktree_root.join(&name);

    // Run the command
    let repos = discover_worktree_repos(&wt_dir).unwrap_or_default();
    let directories: Vec<String> = repos
        .iter()
        .map(|r| r.path.display().to_string())
        .collect();

    let command_str = cmd_parts.join(" ");
    let config = loop_lib::LoopConfig {
        directories,
        ignore: vec![],
        include_filters: None,
        exclude_filters: None,
        verbose,
        silent: false,
        parallel: args.parallel,
        dry_run: false,
        json_output: json,
        add_aliases_to_global_looprc: false,
        spawn_stagger_ms: 0,
    };

    let exec_result = loop_lib::run(&config, &command_str);

    // Destroy worktree regardless of exec result
    if verbose {
        eprintln!("Destroying ephemeral worktree '{name}'...");
    }
    let destroy_args = DestroyArgs {
        name: name.clone(),
        force: true,
    };
    if let Err(e) = handle_destroy(destroy_args, verbose, json) {
        eprintln!(
            "{} Failed to destroy ephemeral worktree '{name}': {e}",
            "warning:".yellow().bold()
        );
        eprintln!(
            "  Run 'meta worktree destroy {name} --force' or 'meta worktree prune' to clean up."
        );
    }

    // Propagate exec result
    exec_result?;
    Ok(())
}

// ==================== Subcommand: prune ====================

#[derive(Debug, Serialize)]
struct PruneOutput {
    removed: Vec<PruneEntry>,
    dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
struct PruneEntry {
    name: String,
    path: String,
    reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    age_seconds: Option<u64>,
}

fn handle_prune(args: PruneArgs, _verbose: bool, json: bool) -> Result<()> {
    let dry_run = args.dry_run;

    let store: WorktreeStoreData = store_list()?;
    if store.worktrees.is_empty() {
        if json {
            println!("{}", serde_json::to_string_pretty(&PruneOutput {
                removed: vec![],
                dry_run,
            })?);
        } else {
            println!("No worktrees in store. Nothing to prune.");
        }
        return Ok(());
    }

    let now = Utc::now().timestamp();
    let mut to_remove: Vec<PruneEntry> = Vec::new();

    for (path_key, entry) in &store.worktrees {
        let wt_path = Path::new(path_key);

        // Check if path exists (orphaned detection)
        if !wt_path.exists() {
            to_remove.push(PruneEntry {
                name: entry.name.clone(),
                path: path_key.clone(),
                reason: "orphaned".to_string(),
                age_seconds: None,
            });
            continue;
        }

        // Check TTL expiration
        if let Some(remaining) = entry_ttl_remaining(entry, now) {
            if remaining <= 0 {
                // age = ttl + overdue time
                let age = (entry.ttl_seconds.unwrap() as i64 - remaining) as u64;
                to_remove.push(PruneEntry {
                    name: entry.name.clone(),
                    path: path_key.clone(),
                    reason: "ttl_expired".to_string(),
                    age_seconds: Some(age),
                });
            }
        }
    }

    if to_remove.is_empty() {
        if json {
            println!("{}", serde_json::to_string_pretty(&PruneOutput {
                removed: vec![],
                dry_run,
            })?);
        } else {
            println!("Nothing to prune.");
        }
        return Ok(());
    }

    if dry_run {
        if json {
            println!("{}", serde_json::to_string_pretty(&PruneOutput {
                removed: to_remove,
                dry_run: true,
            })?);
        } else {
            println!("Would prune {} worktree(s):", to_remove.len());
            for entry in &to_remove {
                println!("  {} ({}) — {}", entry.name, entry.reason, entry.path);
            }
        }
        return Ok(());
    }

    // Actually remove: physical cleanup first, then batch store update.
    // Only remove from store if the directory is actually gone — otherwise the
    // entry would become invisible on subsequent prune runs.
    let mut removed = Vec::new();
    for prune_entry in &to_remove {
        let wt_path = Path::new(&prune_entry.path);

        if wt_path.exists() {
            // Try to properly remove via git worktree remove
            let repos = discover_worktree_repos(wt_path).unwrap_or_default();
            for r in repos.iter().filter(|r| r.alias != ".") {
                let _ = git_worktree_remove(&r.source_path, &r.path, true);
            }
            if let Some(dot_repo) = repos.iter().find(|r| r.alias == ".") {
                let _ = git_worktree_remove(&dot_repo.source_path, &dot_repo.path, true);
            }
            // Clean up directory
            let _ = std::fs::remove_dir_all(wt_path);

            // Only record as removed if directory is actually gone
            if wt_path.exists() {
                eprintln!(
                    "{} Failed to remove directory: {}",
                    "warning:".yellow().bold(),
                    wt_path.display()
                );
                continue;
            }
        }

        removed.push(prune_entry.clone());
    }

    // Batch-remove all pruned entries from store in a single lock cycle
    let keys_to_remove: Vec<String> = removed.iter().map(|e| e.path.clone()).collect();
    let data_path = store_path();
    if data_path.exists() {
        let lock_path = store_lock_path(&data_path);
        if let Err(e) = meta_core::store::update::<WorktreeStoreData, _>(&data_path, &lock_path, |store| {
            for key in &keys_to_remove {
                store.worktrees.remove(key);
            }
        }) {
            eprintln!("{} Failed to update store: {}", "warning:".yellow().bold(), e);
        }
    }

    // Fire post-prune hook
    let meta_dir = find_meta_dir();
    let hook_payload = serde_json::json!({
        "action": "prune",
        "removed": removed.iter().map(|e| serde_json::json!({
            "name": e.name,
            "path": e.path,
            "reason": e.reason,
        })).collect::<Vec<_>>(),
    });
    fire_worktree_hook("post-prune", &hook_payload, meta_dir.as_deref());

    if json {
        println!("{}", serde_json::to_string_pretty(&PruneOutput {
            removed,
            dry_run: false,
        })?);
    } else {
        println!("{} Pruned {} worktree(s):", "✓".green(), removed.len());
        for entry in &removed {
            println!("  {} ({}) — {}", entry.name, entry.reason, entry.path);
        }
    }

    Ok(())
}
