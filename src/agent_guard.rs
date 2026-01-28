//! Deterministic destructive command detection for Claude Code PreToolUse hooks.
//!
//! Reads hook JSON from stdin, evaluates the Bash command for destructive patterns,
//! and returns structured JSON to block or allow execution. No LLM evaluation —
//! pure pattern matching in Rust.
//!
//! Configuration is loaded from `.claude/agent-guard.toml` (project-level) or
//! `~/.claude/agent-guard.toml` (user-level), with embedded defaults as fallback.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

// ── Configuration ───────────────────────────────────────

/// Default agent guard configuration embedded in the binary.
const DEFAULT_CONFIG: &str = include_str!("../../.claude/agent-guard.toml");

/// Cached configuration loaded once per process.
/// This avoids repeated file I/O and TOML parsing on every command evaluation.
static CACHED_CONFIG: OnceLock<GuardConfig> = OnceLock::new();

/// Agent guard configuration structure.
#[derive(Debug, Clone, Deserialize)]
pub struct GuardConfig {
    #[serde(default)]
    pub patterns: PatternConfig,
}

/// Configuration for individual destructive patterns.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PatternConfig {
    #[serde(default)]
    pub git_force_push: PatternRule,
    #[serde(default)]
    pub git_reset_hard: PatternRule,
    #[serde(default)]
    pub git_clean_force: PatternRule,
    #[serde(default)]
    pub git_checkout_dot: PatternRule,
    #[serde(default)]
    pub git_branch_force_delete: PatternRule,
    #[serde(default)]
    pub git_stash_destructive: PatternRule,
    #[serde(default)]
    pub rm_rf_root: PatternRule,
}

/// Individual pattern rule with enable flag and optional custom message.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PatternRule {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub message: Option<String>,
}

fn default_enabled() -> bool {
    true
}

impl GuardConfig {
    /// Load configuration from the hierarchy: project → user → embedded defaults.
    pub fn load() -> Self {
        // Try project-level config first
        if let Some(config) = Self::load_from_project() {
            return config;
        }

        // Try user-level config
        if let Some(config) = Self::load_from_user() {
            return config;
        }

        // Fall back to embedded defaults
        Self::load_from_embedded()
    }

    /// Load config from project-level `.claude/agent-guard.toml`.
    fn load_from_project() -> Option<Self> {
        let path = Path::new(".claude/agent-guard.toml");
        Self::load_from_file(path)
    }

    /// Load config from user-level `~/.claude/agent-guard.toml`.
    fn load_from_user() -> Option<Self> {
        let home = dirs::home_dir()?;
        let path = home.join(".claude/agent-guard.toml");
        Self::load_from_file(&path)
    }

    /// Load config from embedded default string.
    fn load_from_embedded() -> Self {
        toml::from_str(DEFAULT_CONFIG)
            .expect("BUG: embedded default config is invalid TOML")
    }

    /// Load config from a specific file path.
    fn load_from_file(path: &Path) -> Option<Self> {
        let contents = std::fs::read_to_string(path).ok()?;
        toml::from_str(&contents).ok()
    }
}

// ── Public API ──────────────────────────────────────────

/// Entry point for `meta agent guard`.
///
/// Reads PreToolUse hook JSON from stdin, evaluates the command,
/// prints denial JSON to stdout if destructive, exits silently if safe.
pub fn handle_guard() -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;

    let command = match parse_command(&input) {
        Some(cmd) => cmd,
        None => return Ok(()), // No command to evaluate — allow
    };

    if let Some(denial) = evaluate_command(&command) {
        let output = HookOutput {
            hook_specific_output: HookSpecificOutput {
                hook_event_name: "PreToolUse".to_string(),
                permission_decision: "deny".to_string(),
                permission_decision_reason: denial.reason,
            },
        };
        println!("{}", serde_json::to_string(&output)?);
    }

    Ok(())
}

// ── Types ───────────────────────────────────────────────

#[derive(Deserialize)]
struct HookInput {
    tool_input: Option<ToolInput>,
}

#[derive(Deserialize)]
struct ToolInput {
    command: Option<String>,
}

#[derive(Serialize)]
struct HookOutput {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: HookSpecificOutput,
}

#[derive(Serialize)]
struct HookSpecificOutput {
    #[serde(rename = "hookEventName")]
    hook_event_name: String,
    #[serde(rename = "permissionDecision")]
    permission_decision: String,
    #[serde(rename = "permissionDecisionReason")]
    permission_decision_reason: String,
}

/// A denial reason returned when a destructive pattern is detected.
#[derive(Debug, Clone, PartialEq)]
pub struct DenyReason {
    pub reason: String,
}

// ── Input Parsing ───────────────────────────────────────

/// Extract the command string from hook JSON input.
/// Returns None if input is empty, malformed, or missing the command field.
fn parse_command(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let hook_input: HookInput = serde_json::from_str(trimmed).ok()?;
    let command = hook_input.tool_input?.command?;
    if command.trim().is_empty() {
        return None;
    }
    Some(command)
}

// ── Command Evaluation ──────────────────────────────────

/// Type alias for pattern check functions.
type CheckFn = fn(&str) -> Option<DenyReason>;

/// Pattern checker registry entry.
struct PatternChecker {
    name: &'static str,
    check_fn: CheckFn,
    get_rule: fn(&PatternConfig) -> &PatternRule,
}

/// Registry of all pattern checkers.
/// This is the single source of truth for which patterns are checked.
const PATTERN_CHECKERS: &[PatternChecker] = &[
    PatternChecker {
        name: "git_force_push",
        check_fn: check_git_force_push,
        get_rule: |c| &c.git_force_push,
    },
    PatternChecker {
        name: "git_reset_hard",
        check_fn: check_git_reset_hard,
        get_rule: |c| &c.git_reset_hard,
    },
    PatternChecker {
        name: "git_clean_force",
        check_fn: check_git_clean_force,
        get_rule: |c| &c.git_clean_force,
    },
    PatternChecker {
        name: "git_checkout_dot",
        check_fn: check_git_checkout_dot,
        get_rule: |c| &c.git_checkout_dot,
    },
    PatternChecker {
        name: "git_branch_force_delete",
        check_fn: check_git_branch_force_delete,
        get_rule: |c| &c.git_branch_force_delete,
    },
    PatternChecker {
        name: "git_stash_destructive",
        check_fn: check_git_stash_destructive,
        get_rule: |c| &c.git_stash_destructive,
    },
    PatternChecker {
        name: "rm_rf_root",
        check_fn: check_rm_rf_root,
        get_rule: |c| &c.rm_rf_root,
    },
];

/// Evaluate a command string for destructive patterns.
/// Returns a DenyReason if the command should be blocked, None if safe.
///
/// Configuration is loaded once and cached for the lifetime of the process.
pub fn evaluate_command(command: &str) -> Option<DenyReason> {
    let config = CACHED_CONFIG.get_or_init(GuardConfig::load);
    for segment in split_compound_command(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(denial) = evaluate_segment(trimmed, config) {
            return Some(denial);
        }
    }
    None
}

/// Evaluate a single command segment using the config-driven pattern registry.
fn evaluate_segment(segment: &str, config: &GuardConfig) -> Option<DenyReason> {
    for checker in PATTERN_CHECKERS {
        let rule = (checker.get_rule)(&config.patterns);

        if !rule.enabled {
            continue; // Skip disabled patterns
        }

        if let Some(mut denial) = (checker.check_fn)(segment) {
            // Debug logging when META_DEBUG_GUARD is set
            if std::env::var("META_DEBUG_GUARD").is_ok() {
                eprintln!("[agent-guard] Pattern '{}' triggered for: {}", checker.name, segment);
            }

            // Use custom message if provided, otherwise keep default
            if let Some(custom_msg) = &rule.message {
                denial.reason = custom_msg.clone();
            }
            return Some(denial);
        }
    }
    None
}

/// Split a compound command on `&&`, `||`, `;`, and `|` delimiters.
/// Simple split — does not handle quoting. Sufficient for Claude-generated commands.
/// Returns trimmed segments.
fn split_compound_command(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut rest = command;

    loop {
        // Find the earliest delimiter.
        // Order matters: check `||` before `|`, and multi-char before single-char.
        let delimiters: &[&str] = &["||", "&&", ";"];
        let earliest = delimiters
            .iter()
            .filter_map(|d| rest.find(d).map(|pos| (pos, d.len())))
            .min_by_key(|(pos, _)| *pos);

        // Also check for standalone pipe `|` (not part of ||)
        let pipe_pos = find_standalone_pipe(rest);

        // Take whichever delimiter comes first
        let next_delimiter = match (earliest, pipe_pos) {
            (Some((pos1, len1)), Some(pos2)) => {
                if pos2 < pos1 {
                    Some((pos2, 1)) // pipe comes first
                } else {
                    Some((pos1, len1)) // other delimiter comes first
                }
            }
            (Some(delim), None) => Some(delim),
            (None, Some(pos)) => Some((pos, 1)),
            (None, None) => None,
        };

        match next_delimiter {
            Some((pos, len)) => {
                segments.push(rest[..pos].trim());
                rest = &rest[pos + len..];
            }
            None => {
                segments.push(rest.trim());
                break;
            }
        }
    }

    segments
}

/// Find a standalone pipe `|` that is NOT part of `||`.
/// Returns the position of the first such pipe, or None if not found.
fn find_standalone_pipe(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'|' {
            // Check if it's part of ||
            let prev_is_pipe = i > 0 && bytes[i - 1] == b'|';
            let next_is_pipe = i + 1 < bytes.len() && bytes[i + 1] == b'|';
            if !prev_is_pipe && !next_is_pipe {
                return Some(i);
            }
        }
    }
    None
}

// ── Destructive Pattern Checks ──────────────────────────

/// Parse a git command into tokenized words and locate the subcommand.
///
/// Returns (words, subcommand_index) where subcommand_index points to the
/// position after the subcommand in the words array.
///
/// Returns None if the segment doesn't match the expected pattern.
fn parse_git_command<'a>(segment: &'a str, subcommand: &str) -> Option<(Vec<&'a str>, usize)> {
    if !segment.contains("git") || !segment.contains(subcommand) {
        return None;
    }

    let words: Vec<&str> = segment.split_whitespace().collect();
    let git_pos = words.iter().position(|w| *w == "git")?;
    let sub_pos = words.iter().skip(git_pos + 1).position(|w| *w == subcommand)?;
    let sub_abs = git_pos + 1 + sub_pos;

    Some((words, sub_abs))
}

/// Detect `git push --force` or `git push -f`, but NOT `--force-with-lease`.
fn check_git_force_push(segment: &str) -> Option<DenyReason> {
    let (words, push_pos) = parse_git_command(segment, "push")?;

    // Check args after "push" for force flags
    for word in &words[push_pos + 1..] {
        if *word == "--force-with-lease" || word.starts_with("--force-with-lease=") {
            return None; // Safe variant — allow
        }
        if *word == "--force" || *word == "-f" {
            return Some(DenyReason {
                reason: "git push --force in a multi-repo workspace can overwrite history across \
                    multiple repos. Safer alternatives:\n\
                    - git push --force-with-lease (checks for upstream changes)\n\
                    - meta --include <repo> exec -- git push --force (target one repo)\n\
                    - meta git snapshot create <name> before force pushing"
                    .to_string(),
            });
        }
    }

    None
}

/// Detect `git reset --hard`.
fn check_git_reset_hard(segment: &str) -> Option<DenyReason> {
    let (words, reset_pos) = parse_git_command(segment, "reset")?;

    for word in &words[reset_pos + 1..] {
        if *word == "--hard" {
            return Some(DenyReason {
                reason: "git reset --hard destroys uncommitted work. In a multi-repo workspace, \
                    this can silently discard changes across repos. Safer alternatives:\n\
                    - meta git snapshot create <name> (save state first)\n\
                    - meta git snapshot restore <name> (reversible reset)\n\
                    - Target a specific repo: cd <repo> && git reset --hard"
                    .to_string(),
            });
        }
    }

    None
}

/// Detect `git clean -fd`, `-fdx`, `-fxd`, `-f -d`, etc.
fn check_git_clean_force(segment: &str) -> Option<DenyReason> {
    let (words, clean_pos) = parse_git_command(segment, "clean")?;

    // Collect all short-flag characters across all flag arguments after "clean".
    // This handles both combined (-fd) and separate (-f -d) flag styles.
    let mut flag_chars = String::new();
    for word in &words[clean_pos + 1..] {
        if word.starts_with('-') && !word.starts_with("--") {
            flag_chars.push_str(&word[1..]); // Strip leading '-'
        }
    }

    if flag_chars.contains('f') && flag_chars.contains('d') {
        return Some(DenyReason {
            reason: "git clean -fd removes untracked files and directories permanently. \
                In a multi-repo workspace, this affects all repos. Safer alternatives:\n\
                - git clean -nd (dry run — preview what would be removed)\n\
                - meta --include <repo> exec -- git clean -fd (target specific repos)\n\
                - meta git snapshot create <name> before cleaning"
                .to_string(),
        });
    }

    None
}

/// Detect `git checkout .` at workspace scope.
fn check_git_checkout_dot(segment: &str) -> Option<DenyReason> {
    if !segment.contains("git") || !segment.contains("checkout") {
        return None;
    }

    let words: Vec<&str> = segment.split_whitespace().collect();
    let git_pos = words.iter().position(|w| *w == "git")?;
    let checkout_pos = words.iter().skip(git_pos + 1).position(|w| *w == "checkout")?;
    let checkout_abs = git_pos + 1 + checkout_pos;

    // Check if the argument after checkout is "." or "--" followed by "."
    let remaining: Vec<&&str> = words[checkout_abs + 1..].iter().collect();
    let is_dot = match remaining.as_slice() {
        [dot] if **dot == "." => true,
        [dashdash, dot] if **dashdash == "--" && **dot == "." => true,
        _ => false,
    };

    if is_dot {
        return Some(DenyReason {
            reason: "git checkout . reverts all modified files. In a multi-repo workspace, \
                ensure you are in the correct repo directory. Safer alternatives:\n\
                - git checkout -- <specific-file> (target specific files)\n\
                - meta --include <repo> exec -- git checkout . (target one repo)\n\
                - meta git snapshot create <name> before reverting"
                .to_string(),
        });
    }

    None
}

/// Detect `git branch -D` (force delete branch).
fn check_git_branch_force_delete(segment: &str) -> Option<DenyReason> {
    let (words, branch_pos) = parse_git_command(segment, "branch")?;

    // Check for -D flag (force delete)
    for word in &words[branch_pos + 1..] {
        if *word == "-D" {
            return Some(DenyReason {
                reason: "git branch -D force-deletes branches, potentially losing unmerged work. \
                    In a multi-repo workspace, this can affect coordination between repos. Safer alternatives:\n\
                    - git branch -d <branch> (safe delete — only works if merged)\n\
                    - git branch -v to check merge status first\n\
                    - meta git snapshot create <name> before deleting\n\
                    - meta --include <repo> exec -- git branch -D <branch> (target specific repos)"
                    .to_string(),
            });
        }
    }

    None
}

/// Detect `git stash drop` or `git stash clear` (destructive stash operations).
fn check_git_stash_destructive(segment: &str) -> Option<DenyReason> {
    let (words, stash_pos) = parse_git_command(segment, "stash")?;

    // Check if next word is "drop" or "clear"
    if let Some(&subcommand) = words.get(stash_pos + 1) {
        if subcommand == "drop" {
            return Some(DenyReason {
                reason: "git stash drop permanently removes a stash entry. \
                    For AI agents working across repos, this could discard important work. Safer alternatives:\n\
                    - git stash list to review what will be dropped\n\
                    - git stash show <stash> to inspect contents first\n\
                    - git stash apply <stash> instead of pop (preserves the stash)\n\
                    - meta git snapshot create <name> captures all stashes"
                    .to_string(),
            });
        } else if subcommand == "clear" {
            return Some(DenyReason {
                reason: "git stash clear removes ALL stash entries permanently. \
                    In a multi-repo workspace, this could discard important work across repos. Safer alternatives:\n\
                    - git stash list to review what will be cleared\n\
                    - git stash drop <stash> to remove specific entries one at a time\n\
                    - meta git snapshot create <name> before clearing (captures all stashes)\n\
                    - meta --include <repo> exec -- git stash clear (target specific repos)"
                    .to_string(),
            });
        }
    }

    None
}

/// Detect `rm -rf` on workspace/repo root paths.
fn check_rm_rf_root(segment: &str) -> Option<DenyReason> {
    if !segment.contains("rm") {
        return None;
    }

    let words: Vec<&str> = segment.split_whitespace().collect();
    let rm_pos = words.iter().position(|w| *w == "rm")?;

    // Check for -rf or -fr flags (may be combined or separate)
    let args_after_rm = &words[rm_pos + 1..];
    let has_recursive_force = args_after_rm.iter().any(|w| {
        if !w.starts_with('-') || w.starts_with("--") {
            return false;
        }
        w.contains('r') && w.contains('f')
    });

    if !has_recursive_force {
        return None;
    }

    // Check if any path argument is a dangerous root-like path
    for word in args_after_rm {
        if word.starts_with('-') {
            continue; // Skip flags
        }
        if is_dangerous_rm_target(word) {
            return Some(DenyReason {
                reason: format!(
                    "rm -rf on '{word}' could destroy repo roots or workspace data. \
                    In a multi-repo workspace, this is especially dangerous. Safer alternatives:\n\
                    - Remove specific files instead of entire directories\n\
                    - meta --dry-run exec -- <cmd> to preview operations\n\
                    - meta git snapshot create <name> before destructive operations"
                ),
            });
        }
    }

    None
}

/// Check if a path target is dangerous for rm -rf.
fn is_dangerous_rm_target(path: &str) -> bool {
    let path = path.trim_end_matches('/');

    // Root filesystem (/ or ///)
    if path.is_empty() {
        return true;
    }

    // Home directory
    if path == "~" || path == "$HOME" {
        return true;
    }

    // Current directory or parent
    if path == "." || path == ".." {
        return true;
    }

    // Paths that are workspace markers
    if path == ".meta" || path == ".meta.yaml" || path == ".meta.yml" {
        return true;
    }

    // Wildcard at root level
    if path == "*" || path == "./*" || path == "../*" {
        return true;
    }

    false
}

// ── Tests ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_command ──────────────────────────────────

    #[test]
    fn parse_command_extracts_command() {
        let input = r#"{"tool_input": {"command": "git status"}}"#;
        assert_eq!(parse_command(input), Some("git status".to_string()));
    }

    #[test]
    fn parse_command_returns_none_for_empty_input() {
        assert_eq!(parse_command(""), None);
        assert_eq!(parse_command("  "), None);
    }

    #[test]
    fn parse_command_returns_none_for_malformed_json() {
        assert_eq!(parse_command("not json"), None);
        assert_eq!(parse_command("{"), None);
    }

    #[test]
    fn parse_command_returns_none_for_missing_fields() {
        assert_eq!(parse_command(r#"{}"#), None);
        assert_eq!(parse_command(r#"{"tool_input": {}}"#), None);
        assert_eq!(
            parse_command(r#"{"tool_input": {"command": ""}}"#),
            None
        );
    }

    // ── split_compound_command ─────────────────────────

    #[test]
    fn split_simple_command() {
        assert_eq!(split_compound_command("git status"), vec!["git status"]);
    }

    #[test]
    fn split_and_chain() {
        assert_eq!(
            split_compound_command("git add . && git commit -m msg"),
            vec!["git add .", "git commit -m msg"]
        );
    }

    #[test]
    fn split_or_chain() {
        assert_eq!(
            split_compound_command("cmd1 || cmd2"),
            vec!["cmd1", "cmd2"]
        );
    }

    #[test]
    fn split_semicolon() {
        assert_eq!(
            split_compound_command("cmd1; cmd2"),
            vec!["cmd1", "cmd2"]
        );
    }

    #[test]
    fn split_mixed_delimiters() {
        assert_eq!(
            split_compound_command("cmd1 && cmd2; cmd3 || cmd4"),
            vec!["cmd1", "cmd2", "cmd3", "cmd4"]
        );
    }

    // ── git push --force ──────────────────────────────

    #[test]
    fn denies_git_push_force() {
        assert!(evaluate_command("git push --force origin main").is_some());
    }

    #[test]
    fn denies_git_push_f() {
        assert!(evaluate_command("git push -f origin main").is_some());
    }

    #[test]
    fn allows_git_push_force_with_lease() {
        assert!(evaluate_command("git push --force-with-lease origin main").is_none());
    }

    #[test]
    fn allows_git_push_force_with_lease_equals() {
        assert!(evaluate_command("git push --force-with-lease=main origin main").is_none());
    }

    #[test]
    fn allows_normal_git_push() {
        assert!(evaluate_command("git push origin main").is_none());
    }

    #[test]
    fn allows_git_push_no_force() {
        assert!(evaluate_command("git push").is_none());
    }

    // ── git reset --hard ──────────────────────────────

    #[test]
    fn denies_git_reset_hard() {
        assert!(evaluate_command("git reset --hard").is_some());
    }

    #[test]
    fn denies_git_reset_hard_with_ref() {
        assert!(evaluate_command("git reset --hard HEAD~3").is_some());
    }

    #[test]
    fn allows_git_reset_soft() {
        assert!(evaluate_command("git reset --soft HEAD~1").is_none());
    }

    #[test]
    fn allows_git_reset_no_flag() {
        assert!(evaluate_command("git reset HEAD file.txt").is_none());
    }

    // ── git clean ─────────────────────────────────────

    #[test]
    fn denies_git_clean_fd() {
        assert!(evaluate_command("git clean -fd").is_some());
    }

    #[test]
    fn denies_git_clean_fdx() {
        assert!(evaluate_command("git clean -fdx").is_some());
    }

    #[test]
    fn denies_git_clean_fxd() {
        assert!(evaluate_command("git clean -fxd").is_some());
    }

    #[test]
    fn denies_git_clean_df() {
        assert!(evaluate_command("git clean -df").is_some());
    }

    #[test]
    fn allows_git_clean_dry_run() {
        assert!(evaluate_command("git clean -nd").is_none());
    }

    #[test]
    fn allows_git_clean_no_force() {
        assert!(evaluate_command("git clean -n").is_none());
    }

    // ── git checkout . ────────────────────────────────

    #[test]
    fn denies_git_checkout_dot() {
        assert!(evaluate_command("git checkout .").is_some());
    }

    #[test]
    fn denies_git_checkout_dashdash_dot() {
        assert!(evaluate_command("git checkout -- .").is_some());
    }

    #[test]
    fn allows_git_checkout_branch() {
        assert!(evaluate_command("git checkout main").is_none());
    }

    #[test]
    fn allows_git_checkout_specific_file() {
        assert!(evaluate_command("git checkout -- src/main.rs").is_none());
    }

    #[test]
    fn allows_git_checkout_b() {
        assert!(evaluate_command("git checkout -b feature/new").is_none());
    }

    // ── rm -rf ────────────────────────────────────────

    #[test]
    fn denies_rm_rf_dot() {
        assert!(evaluate_command("rm -rf .").is_some());
    }

    #[test]
    fn denies_rm_rf_parent() {
        assert!(evaluate_command("rm -rf ..").is_some());
    }

    #[test]
    fn denies_rm_rf_slash() {
        assert!(evaluate_command("rm -rf /").is_some());
    }

    #[test]
    fn denies_rm_rf_meta() {
        assert!(evaluate_command("rm -rf .meta").is_some());
    }

    #[test]
    fn denies_rm_rf_star() {
        assert!(evaluate_command("rm -rf *").is_some());
    }

    #[test]
    fn denies_rm_fr_dot() {
        assert!(evaluate_command("rm -fr .").is_some());
    }

    #[test]
    fn allows_rm_rf_specific_dir() {
        assert!(evaluate_command("rm -rf node_modules").is_none());
    }

    #[test]
    fn allows_rm_rf_specific_path() {
        assert!(evaluate_command("rm -rf target/debug").is_none());
    }

    #[test]
    fn allows_rm_without_rf() {
        assert!(evaluate_command("rm file.txt").is_none());
    }

    // ── Compound commands ─────────────────────────────

    #[test]
    fn denies_destructive_in_compound() {
        assert!(evaluate_command("git add . && git push --force").is_some());
    }

    #[test]
    fn allows_safe_compound() {
        assert!(evaluate_command("git add . && git commit -m msg && git push").is_none());
    }

    #[test]
    fn denies_second_segment_in_semicolon() {
        assert!(evaluate_command("echo hi; git reset --hard").is_some());
    }

    // ── Safe commands ─────────────────────────────────

    #[test]
    fn allows_git_status() {
        assert!(evaluate_command("git status").is_none());
    }

    #[test]
    fn allows_cargo_build() {
        assert!(evaluate_command("cargo build").is_none());
    }

    #[test]
    fn allows_ls() {
        assert!(evaluate_command("ls -la").is_none());
    }

    #[test]
    fn allows_meta_commands() {
        assert!(evaluate_command("meta git status").is_none());
        assert!(evaluate_command("meta exec -- cargo test").is_none());
    }

    // ── Denial reason content ─────────────────────────

    #[test]
    fn force_push_reason_suggests_lease() {
        let denial = evaluate_command("git push --force").unwrap();
        assert!(denial.reason.contains("--force-with-lease"));
    }

    #[test]
    fn reset_hard_reason_suggests_snapshot() {
        let denial = evaluate_command("git reset --hard").unwrap();
        assert!(denial.reason.contains("snapshot"));
    }

    #[test]
    fn clean_reason_suggests_dry_run() {
        let denial = evaluate_command("git clean -fd").unwrap();
        assert!(denial.reason.contains("-nd"));
    }

    // ── JSON output structure ─────────────────────────

    #[test]
    fn hook_output_serializes_correctly() {
        let output = HookOutput {
            hook_specific_output: HookSpecificOutput {
                hook_event_name: "PreToolUse".to_string(),
                permission_decision: "deny".to_string(),
                permission_decision_reason: "test reason".to_string(),
            },
        };
        let json = serde_json::to_string(&output).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["hookEventName"],
            "PreToolUse"
        );
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecision"],
            "deny"
        );
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecisionReason"],
            "test reason"
        );
    }

    // ── Pipe delimiter ───────────────────────────────

    #[test]
    fn split_pipe_delimiter() {
        assert_eq!(
            split_compound_command("git push --force | tee log.txt"),
            vec!["git push --force", "tee log.txt"]
        );
    }

    #[test]
    fn denies_force_push_piped() {
        assert!(evaluate_command("git push --force origin main | tee output.log").is_some());
    }

    #[test]
    fn denies_reset_hard_piped() {
        assert!(evaluate_command("git reset --hard | cat").is_some());
    }

    #[test]
    fn split_pipe_does_not_confuse_or() {
        // " || " should be matched as OR, not as two pipes
        assert_eq!(
            split_compound_command("cmd1 || cmd2"),
            vec!["cmd1", "cmd2"]
        );
    }

    // ── git clean separate flags ─────────────────────

    #[test]
    fn denies_git_clean_f_d_separate() {
        assert!(evaluate_command("git clean -f -d").is_some());
    }

    #[test]
    fn denies_git_clean_d_f_separate() {
        assert!(evaluate_command("git clean -d -f").is_some());
    }

    #[test]
    fn denies_git_clean_f_d_x_separate() {
        assert!(evaluate_command("git clean -f -d -x").is_some());
    }

    #[test]
    fn allows_git_clean_f_only() {
        // -f alone without -d should be allowed (only removes files, not dirs)
        assert!(evaluate_command("git clean -f").is_none());
    }

    // ── rm -rf edge cases ────────────────────────────

    #[test]
    fn denies_rm_rf_meta_yaml() {
        assert!(evaluate_command("rm -rf .meta.yaml").is_some());
    }

    #[test]
    fn denies_rm_rf_meta_yml() {
        assert!(evaluate_command("rm -rf .meta.yml").is_some());
    }

    #[test]
    fn denies_rm_rf_home_tilde() {
        assert!(evaluate_command("rm -rf ~").is_some());
    }

    #[test]
    fn denies_rm_rf_home_var() {
        assert!(evaluate_command("rm -rf $HOME").is_some());
    }

    #[test]
    fn denies_rm_rf_dot_star() {
        assert!(evaluate_command("rm -rf ./*").is_some());
    }

    #[test]
    fn denies_rm_rf_parent_star() {
        assert!(evaluate_command("rm -rf ../*").is_some());
    }

    #[test]
    fn denies_rm_rf_trailing_slash() {
        assert!(evaluate_command("rm -rf ./").is_some());
    }

    #[test]
    fn denies_rm_rf_multiple_targets_with_dangerous() {
        // Should catch .meta even among safe targets
        assert!(evaluate_command("rm -rf node_modules .meta target").is_some());
    }

    // ── parse_command edge cases ─────────────────────

    #[test]
    fn parse_command_handles_null_tool_input() {
        assert_eq!(parse_command(r#"{"tool_input": null}"#), None);
    }

    #[test]
    fn parse_command_handles_null_command() {
        assert_eq!(
            parse_command(r#"{"tool_input": {"command": null}}"#),
            None
        );
    }

    #[test]
    fn parse_command_ignores_extra_fields() {
        let input = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git status","description":"check status"},"session_id":"abc"}"#;
        assert_eq!(parse_command(input), Some("git status".to_string()));
    }

    // ── git branch -D ────────────────────────────────────

    #[test]
    fn denies_git_branch_force_delete() {
        assert!(evaluate_command("git branch -D feature-branch").is_some());
    }

    #[test]
    fn denies_git_branch_force_delete_multiple() {
        assert!(evaluate_command("git branch -D feat1 feat2").is_some());
    }

    #[test]
    fn allows_git_branch_safe_delete() {
        assert!(evaluate_command("git branch -d feature-branch").is_none());
    }

    #[test]
    fn allows_git_branch_list() {
        assert!(evaluate_command("git branch").is_none());
        assert!(evaluate_command("git branch -v").is_none());
        assert!(evaluate_command("git branch -a").is_none());
    }

    #[test]
    fn allows_git_branch_create() {
        assert!(evaluate_command("git branch new-feature").is_none());
    }

    #[test]
    fn branch_delete_reason_suggests_safe_alternative() {
        let denial = evaluate_command("git branch -D old-branch").unwrap();
        assert!(denial.reason.contains("git branch -d"));
        assert!(denial.reason.contains("safe delete"));
    }

    // ── git stash drop/clear ──────────────────────────

    #[test]
    fn denies_git_stash_drop() {
        assert!(evaluate_command("git stash drop").is_some());
    }

    #[test]
    fn denies_git_stash_drop_with_ref() {
        assert!(evaluate_command("git stash drop stash@{0}").is_some());
    }

    #[test]
    fn denies_git_stash_clear() {
        assert!(evaluate_command("git stash clear").is_some());
    }

    #[test]
    fn allows_git_stash() {
        assert!(evaluate_command("git stash").is_none());
    }

    #[test]
    fn allows_git_stash_push() {
        assert!(evaluate_command("git stash push -m 'WIP'").is_none());
    }

    #[test]
    fn allows_git_stash_list() {
        assert!(evaluate_command("git stash list").is_none());
    }

    #[test]
    fn allows_git_stash_show() {
        assert!(evaluate_command("git stash show").is_none());
        assert!(evaluate_command("git stash show stash@{0}").is_none());
    }

    #[test]
    fn allows_git_stash_apply() {
        assert!(evaluate_command("git stash apply").is_none());
        assert!(evaluate_command("git stash apply stash@{1}").is_none());
    }

    #[test]
    fn allows_git_stash_pop() {
        assert!(evaluate_command("git stash pop").is_none());
    }

    #[test]
    fn stash_drop_reason_suggests_alternatives() {
        let denial = evaluate_command("git stash drop").unwrap();
        assert!(denial.reason.contains("git stash list"));
        assert!(denial.reason.contains("git stash apply"));
    }

    #[test]
    fn stash_clear_reason_suggests_alternatives() {
        let denial = evaluate_command("git stash clear").unwrap();
        assert!(denial.reason.contains("ALL stash entries"));
        assert!(denial.reason.contains("git stash drop"));
    }

    // ── Pipe handling without spaces ──────────────────

    #[test]
    fn split_pipe_no_spaces() {
        assert_eq!(
            split_compound_command("git status|tee log.txt"),
            vec!["git status", "tee log.txt"]
        );
    }

    #[test]
    fn denies_force_push_piped_no_spaces() {
        assert!(evaluate_command("git push --force|tee output.log").is_some());
    }

    #[test]
    fn denies_reset_hard_piped_no_spaces() {
        assert!(evaluate_command("git reset --hard|cat").is_some());
    }

    #[test]
    fn split_pipe_mixed_spacing() {
        assert_eq!(
            split_compound_command("cmd1|cmd2 | cmd3"),
            vec!["cmd1", "cmd2", "cmd3"]
        );
    }

    #[test]
    fn split_does_not_break_or_without_spaces() {
        // "cmd1||cmd2" should still be treated as OR (not two pipes)
        assert_eq!(
            split_compound_command("cmd1||cmd2"),
            vec!["cmd1", "cmd2"]
        );
    }

    #[test]
    fn pipe_in_compound_with_destructive() {
        assert!(evaluate_command("git add .|git commit -m msg && git push --force").is_some());
    }

    // ── Edge cases for new patterns ───────────────────

    #[test]
    fn compound_with_branch_delete() {
        assert!(evaluate_command("git checkout main && git branch -D old-feature").is_some());
    }

    #[test]
    fn compound_with_stash_clear() {
        assert!(evaluate_command("git stash && git stash clear").is_some());
    }

    #[test]
    fn all_new_patterns_in_one_chain() {
        assert!(evaluate_command("git branch -D feat1 && git stash drop && git reset --hard").is_some());
    }

    // ── Configuration loading tests ────────────────────

    #[test]
    fn config_loads_embedded_defaults() {
        let config = GuardConfig::load_from_embedded();
        assert!(config.patterns.git_force_push.enabled);
        assert!(config.patterns.git_reset_hard.enabled);
        assert!(config.patterns.git_branch_force_delete.enabled);
        assert!(config.patterns.git_stash_destructive.enabled);
    }

    #[test]
    fn config_default_patterns_are_enabled() {
        let config = GuardConfig::load();
        // All default patterns should be enabled
        assert!(config.patterns.git_force_push.enabled);
        assert!(config.patterns.git_reset_hard.enabled);
        assert!(config.patterns.git_clean_force.enabled);
        assert!(config.patterns.git_checkout_dot.enabled);
        assert!(config.patterns.git_branch_force_delete.enabled);
        assert!(config.patterns.git_stash_destructive.enabled);
        assert!(config.patterns.rm_rf_root.enabled);
    }

    #[test]
    fn config_can_parse_custom_toml() {
        let toml = r#"
[patterns.git_force_push]
enabled = false

[patterns.git_reset_hard]
enabled = true
message = "Custom reset message"
"#;
        let config: GuardConfig = toml::from_str(toml).unwrap();
        assert!(!config.patterns.git_force_push.enabled);
        assert!(config.patterns.git_reset_hard.enabled);
        assert_eq!(
            config.patterns.git_reset_hard.message.as_ref().unwrap(),
            "Custom reset message"
        );
    }

    #[test]
    fn disabled_pattern_is_not_checked() {
        // Create a custom config with git_force_push disabled
        let toml = r#"
[patterns.git_force_push]
enabled = false
"#;
        let config: GuardConfig = toml::from_str(toml).unwrap();

        // This command should normally be denied, but with the pattern disabled it should pass
        let result = evaluate_segment("git push --force origin main", &config);
        assert!(result.is_none());
    }

    #[test]
    fn custom_message_overrides_default() {
        let toml = r#"
[patterns.git_force_push]
enabled = true
message = "TEAM POLICY: No force push ever!"
"#;
        let config: GuardConfig = toml::from_str(toml).unwrap();
        let result = evaluate_segment("git push --force", &config).unwrap();
        assert_eq!(result.reason, "TEAM POLICY: No force push ever!");
    }

    #[test]
    fn pattern_checker_registry_covers_all_patterns() {
        // Ensure the registry has an entry for each pattern
        let pattern_names: Vec<&str> = PATTERN_CHECKERS.iter().map(|c| c.name).collect();
        assert!(pattern_names.contains(&"git_force_push"));
        assert!(pattern_names.contains(&"git_reset_hard"));
        assert!(pattern_names.contains(&"git_clean_force"));
        assert!(pattern_names.contains(&"git_checkout_dot"));
        assert!(pattern_names.contains(&"git_branch_force_delete"));
        assert!(pattern_names.contains(&"git_stash_destructive"));
        assert!(pattern_names.contains(&"rm_rf_root"));
    }

    #[test]
    fn config_is_cached_across_evaluations() {
        // First evaluation loads config
        let result1 = evaluate_command("git status");
        assert!(result1.is_none());

        // Second evaluation should use cached config (no additional file I/O)
        let result2 = evaluate_command("git push --force");
        assert!(result2.is_some());

        // Verify both evaluations worked correctly
        let result3 = evaluate_command("git branch -D test");
        assert!(result3.is_some());
    }

    #[test]
    fn debug_logging_available() {
        // Test that debug env var is checked (doesn't crash)
        std::env::set_var("META_DEBUG_GUARD", "1");
        let result = evaluate_command("git push --force");
        assert!(result.is_some());
        std::env::remove_var("META_DEBUG_GUARD");
    }
}
