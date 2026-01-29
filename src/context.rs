//! Workspace context summary for `meta context`.
//!
//! Outputs a structured summary of the workspace: repos, branches, dirty status,
//! tags, dependencies. Designed for both humans and LLM agents (injected via
//! Claude Code SessionStart hook).

use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::config::{self, ProjectInfo};
use crate::dependency_graph::DependencyGraph;
use crate::git_utils;

// ── Cache ───────────────────────────────────────────────

const CACHE_TTL_SECONDS: u64 = 30;

#[derive(Debug, Serialize, Deserialize)]
struct CachedContext {
    context: WorkspaceContext,
    timestamp: SystemTime,
    workspace_root: PathBuf,
}

fn cache_path() -> PathBuf {
    meta_core::data_dir::data_file("context_cache")
}

fn load_cache() -> Option<CachedContext> {
    let path = cache_path();
    let content = std::fs::read(&path).ok()?;
    serde_json::from_slice(&content).ok()
}

fn save_cache(cached: &CachedContext) {
    let path = cache_path();
    if let Ok(json) = serde_json::to_vec(cached) {
        let _ = std::fs::write(path, json);
    }
}

fn is_cache_valid(cached: &CachedContext, current_root: &PathBuf) -> bool {
    // Check workspace root matches
    if cached.workspace_root != *current_root {
        return false;
    }

    // Check TTL
    if let Ok(elapsed) = SystemTime::now().duration_since(cached.timestamp) {
        elapsed < Duration::from_secs(CACHE_TTL_SECONDS)
    } else {
        false
    }
}

// ── Public API ──────────────────────────────────────────

/// Entry point for `meta context`.
pub fn handle_context(json: bool, no_status: bool, no_cache: bool, verbose: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("Failed to get current directory")?;

    let (config_path, _format) = config::find_meta_config(&cwd, None)
        .ok_or_else(|| anyhow::anyhow!("Not a meta workspace (no .meta config found)"))?;

    let meta_dir = config_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid config path"))?
        .to_path_buf();

    // Try cache if not bypassed
    if !no_cache && !no_status {
        if let Some(cached) = load_cache() {
            if is_cache_valid(&cached, &meta_dir) {
                if verbose {
                    eprintln!("Using cached context (age < {}s)", CACHE_TTL_SECONDS);
                }
                if json {
                    println!("{}", serde_json::to_string_pretty(&cached.context)?);
                } else {
                    print!("{}", format_markdown(&cached.context));
                }
                return Ok(());
            } else if verbose {
                eprintln!("Cache expired or invalid, regenerating...");
            }
        }
    }

    let workspace_name = meta_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let (projects, _ignore_list) = config::parse_meta_config(&config_path)?;

    if verbose {
        eprintln!(
            "Found {} projects in {}",
            projects.len(),
            config_path.display()
        );
    }

    let repos: Vec<RepoContext> = if no_status {
        projects.iter().map(RepoContext::from_project).collect()
    } else {
        projects
            .par_iter()
            .map(|p| {
                let mut ctx = RepoContext::from_project(p);
                let repo_path = meta_dir.join(&p.path);
                if repo_path.exists() {
                    ctx.branch = git_utils::current_branch(&repo_path);
                    ctx.dirty = git_utils::is_dirty(&repo_path);
                    ctx.modified_count = git_utils::dirty_file_count(&repo_path);

                    // Get ahead/behind counts
                    if let Some((ahead, behind)) = git_utils::ahead_behind(&repo_path) {
                        ctx.ahead = Some(ahead);
                        ctx.behind = Some(behind);
                    }
                }
                ctx
            })
            .collect()
    };

    let dependencies = build_dependency_map(&projects);

    let ctx = WorkspaceContext {
        name: workspace_name,
        description: "Multi-repo workspace managed by `meta`. Use `meta` commands for cross-repo operations.".to_string(),
        repo_count: repos.len(),
        repos,
        commands: key_commands(),
        dependencies,
    };

    // Save to cache (only if status was collected and cache wasn't bypassed)
    if !no_cache && !no_status {
        let cached = CachedContext {
            context: ctx.clone(),
            timestamp: SystemTime::now(),
            workspace_root: meta_dir,
        };
        save_cache(&cached);
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&ctx)?);
    } else {
        print!("{}", format_markdown(&ctx));
    }

    Ok(())
}

// ── Types ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceContext {
    pub name: String,
    pub description: String,
    pub repo_count: usize,
    pub repos: Vec<RepoContext>,
    pub commands: Vec<CommandRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<HashMap<String, Vec<String>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRef {
    pub command: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoContext {
    pub name: String,
    pub path: String,
    pub repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<usize>,
    pub tags: Vec<String>,
}

impl RepoContext {
    /// Create a RepoContext from a ProjectInfo with no git status.
    pub fn from_project(p: &ProjectInfo) -> Self {
        Self {
            name: p.name.clone(),
            path: p.path.clone(),
            repo: p.repo.clone(),
            branch: None,
            dirty: None,
            modified_count: None,
            ahead: None,
            behind: None,
            tags: p.tags.clone(),
        }
    }
}

fn key_commands() -> Vec<CommandRef> {
    vec![
        CommandRef {
            command: "meta git status".to_string(),
            description: "all repos at once".to_string(),
        },
        CommandRef {
            command: "meta exec -- <cmd>".to_string(),
            description: "run in all repos".to_string(),
        },
        CommandRef {
            command: "meta git commit -m \"msg\"".to_string(),
            description: "commit in all dirty repos".to_string(),
        },
        CommandRef {
            command: "meta git snapshot create <name>".to_string(),
            description: "save state before batch changes".to_string(),
        },
        CommandRef {
            command: "meta --include X,Y exec -- <cmd>".to_string(),
            description: "target specific repos".to_string(),
        },
    ]
}

// ── Dependency Graph ────────────────────────────────────

fn build_dependency_map(projects: &[ProjectInfo]) -> Option<HashMap<String, Vec<String>>> {
    let has_deps = projects.iter().any(|p| !p.depends_on.is_empty());
    if !has_deps {
        return None;
    }

    let dep_projects: Vec<_> = projects.iter().map(|p| p.to_dependencies()).collect();
    let graph = DependencyGraph::build(dep_projects).ok()?;

    let mut map = HashMap::new();
    for project in projects {
        let deps = graph.get_dependencies(&project.name);
        if !deps.is_empty() {
            map.insert(
                project.name.clone(),
                deps.iter().map(|s| s.to_string()).collect(),
            );
        }
    }

    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

// ── Markdown Formatting ─────────────────────────────────

pub fn format_markdown(ctx: &WorkspaceContext) -> String {
    let mut out = String::new();

    // Header
    out.push_str(&format!(
        "# Meta Workspace: {} ({} repos)\n\n",
        ctx.name, ctx.repo_count
    ));
    out.push_str(&ctx.description);
    out.push_str("\n\n");

    // Repo table
    let has_status = ctx.repos.iter().any(|r| r.branch.is_some());
    let has_tags = ctx.repos.iter().any(|r| !r.tags.is_empty());

    if has_status && has_tags {
        out.push_str("## Repos\n");
        out.push_str("| Repo | Branch | Status | Tags |\n");
        out.push_str("|------|--------|--------|------|\n");
        for r in &ctx.repos {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                r.name,
                r.branch.as_deref().unwrap_or("-"),
                format_status(r),
                r.tags.join(", "),
            ));
        }
    } else if has_status {
        out.push_str("## Repos\n");
        out.push_str("| Repo | Branch | Status |\n");
        out.push_str("|------|--------|--------|\n");
        for r in &ctx.repos {
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                r.name,
                r.branch.as_deref().unwrap_or("-"),
                format_status(r),
            ));
        }
    } else {
        out.push_str("## Repos\n");
        if has_tags {
            out.push_str("| Repo | Tags |\n");
            out.push_str("|------|------|\n");
            for r in &ctx.repos {
                out.push_str(&format!("| {} | {} |\n", r.name, r.tags.join(", ")));
            }
        } else {
            for r in &ctx.repos {
                out.push_str(&format!("- {}\n", r.name));
            }
        }
    }

    // Key commands
    out.push_str("\n## Key Commands\n");
    for cmd in &ctx.commands {
        out.push_str(&format!("- `{}` — {}\n", cmd.command, cmd.description));
    }

    // Dependencies
    if let Some(ref deps) = ctx.dependencies {
        out.push_str("\n## Dependencies\n");
        let mut sorted_keys: Vec<&String> = deps.keys().collect();
        sorted_keys.sort();
        for key in sorted_keys {
            let targets = &deps[key];
            out.push_str(&format!("{} → {}\n", key, targets.join(", ")));
        }
    }

    out
}

fn format_status(r: &RepoContext) -> String {
    let base = match (r.dirty, r.modified_count) {
        (Some(false), _) => "clean".to_string(),
        (Some(true), Some(n)) => format!("{n} modified"),
        (Some(true), None) => "dirty".to_string(),
        _ => "-".to_string(),
    };

    // Add ahead/behind indicator
    match (r.ahead, r.behind) {
        (Some(a), Some(b)) if a > 0 && b > 0 => format!("{} (↑{} ↓{})", base, a, b),
        (Some(a), _) if a > 0 => format!("{} (↑{})", base, a),
        (_, Some(b)) if b > 0 => format!("{} (↓{})", base, b),
        _ => base,
    }
}

// ── Tests ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx(repos: Vec<RepoContext>, deps: Option<HashMap<String, Vec<String>>>) -> WorkspaceContext {
        WorkspaceContext {
            name: "test-workspace".to_string(),
            description: "Multi-repo workspace managed by `meta`. Use `meta` commands for cross-repo operations.".to_string(),
            repo_count: repos.len(),
            repos,
            commands: key_commands(),
            dependencies: deps,
        }
    }

    fn make_repo(name: &str, branch: Option<&str>, dirty: Option<bool>, modified: Option<usize>, tags: Vec<&str>) -> RepoContext {
        RepoContext {
            name: name.to_string(),
            path: name.to_string(),
            repo: format!("git@github.com:org/{name}.git"),
            branch: branch.map(|s| s.to_string()),
            dirty,
            modified_count: modified,
            ahead: None,
            behind: None,
            tags: tags.into_iter().map(|s| s.to_string()).collect(),
        }
    }

    // ── format_status ──────────────────────────────────

    #[test]
    fn status_clean() {
        let r = make_repo("x", Some("main"), Some(false), Some(0), vec![]);
        assert_eq!(format_status(&r), "clean");
    }

    #[test]
    fn status_dirty_with_count() {
        let r = make_repo("x", Some("main"), Some(true), Some(3), vec![]);
        assert_eq!(format_status(&r), "3 modified");
    }

    #[test]
    fn status_dirty_no_count() {
        let r = make_repo("x", Some("main"), Some(true), None, vec![]);
        assert_eq!(format_status(&r), "dirty");
    }

    #[test]
    fn status_unknown() {
        let r = make_repo("x", None, None, None, vec![]);
        assert_eq!(format_status(&r), "-");
    }

    // ── format_markdown ─────────────────────────────────

    #[test]
    fn markdown_includes_header() {
        let ctx = make_ctx(
            vec![make_repo("lib", Some("main"), Some(false), Some(0), vec![])],
            None,
        );
        let md = format_markdown(&ctx);
        assert!(md.contains("# Meta Workspace: test-workspace (1 repos)"));
        assert!(md.contains("Multi-repo workspace"));
    }

    #[test]
    fn markdown_includes_repo_table_with_status() {
        let ctx = make_ctx(
            vec![
                make_repo("api", Some("main"), Some(false), Some(0), vec![]),
                make_repo("web", Some("feat-x"), Some(true), Some(2), vec![]),
            ],
            None,
        );
        let md = format_markdown(&ctx);
        assert!(md.contains("| Repo | Branch | Status |"));
        assert!(md.contains("| api | main | clean |"));
        assert!(md.contains("| web | feat-x | 2 modified |"));
    }

    #[test]
    fn markdown_includes_tags_column_when_present() {
        let ctx = make_ctx(
            vec![make_repo("api", Some("main"), Some(false), Some(0), vec!["backend"])],
            None,
        );
        let md = format_markdown(&ctx);
        assert!(md.contains("| Tags |"));
        assert!(md.contains("| backend |"));
    }

    #[test]
    fn markdown_no_status_shows_simple_list() {
        let ctx = make_ctx(
            vec![
                make_repo("api", None, None, None, vec![]),
                make_repo("web", None, None, None, vec![]),
            ],
            None,
        );
        let md = format_markdown(&ctx);
        assert!(md.contains("- api"));
        assert!(md.contains("- web"));
    }

    #[test]
    fn markdown_includes_key_commands() {
        let ctx = make_ctx(vec![], None);
        let md = format_markdown(&ctx);
        assert!(md.contains("## Key Commands"));
        assert!(md.contains("meta git status"));
        assert!(md.contains("meta exec"));
    }

    #[test]
    fn markdown_includes_dependencies_when_present() {
        let mut deps = HashMap::new();
        deps.insert("api".to_string(), vec!["core".to_string()]);
        let ctx = make_ctx(vec![], Some(deps));
        let md = format_markdown(&ctx);
        assert!(md.contains("## Dependencies"));
        assert!(md.contains("api → core"));
    }

    #[test]
    fn markdown_omits_dependencies_when_none() {
        let ctx = make_ctx(vec![], None);
        let md = format_markdown(&ctx);
        assert!(!md.contains("## Dependencies"));
    }

    // ── JSON serialization ──────────────────────────────

    #[test]
    fn json_serializes_full_context() {
        let ctx = make_ctx(
            vec![make_repo("api", Some("main"), Some(false), Some(0), vec!["backend"])],
            None,
        );
        let json = serde_json::to_string(&ctx).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["name"], "test-workspace");
        assert_eq!(v["repo_count"], 1);
        assert_eq!(v["repos"][0]["name"], "api");
        assert_eq!(v["repos"][0]["path"], "api");
        assert_eq!(v["repos"][0]["repo"], "git@github.com:org/api.git");
        assert_eq!(v["repos"][0]["branch"], "main");
        assert_eq!(v["repos"][0]["dirty"], false);
        assert_eq!(v["repos"][0]["tags"][0], "backend");
    }

    #[test]
    fn json_omits_none_fields() {
        let ctx = make_ctx(
            vec![make_repo("api", None, None, None, vec![])],
            None,
        );
        let json = serde_json::to_string(&ctx).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["repos"][0].get("branch").is_none());
        assert!(v["repos"][0].get("dirty").is_none());
        assert!(v["repos"][0].get("modified_count").is_none());
        assert!(v.get("dependencies").is_none());
    }

    #[test]
    fn json_includes_description_and_commands() {
        let ctx = make_ctx(vec![], None);
        let json = serde_json::to_string(&ctx).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["description"].as_str().unwrap().contains("meta"));
        let commands = v["commands"].as_array().unwrap();
        assert!(!commands.is_empty());
        assert!(commands[0].get("command").is_some());
        assert!(commands[0].get("description").is_some());
    }

    #[test]
    fn json_includes_dependencies_when_present() {
        let mut deps = HashMap::new();
        deps.insert("api".to_string(), vec!["core".to_string()]);
        let ctx = make_ctx(vec![], Some(deps));
        let json = serde_json::to_string(&ctx).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["dependencies"]["api"][0], "core");
    }
}
