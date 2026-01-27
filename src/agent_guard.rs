//! Deterministic destructive command detection for Claude Code PreToolUse hooks.
//!
//! Reads hook JSON from stdin, evaluates the Bash command for destructive patterns,
//! and returns structured JSON to block or allow execution. No LLM evaluation —
//! pure pattern matching in Rust.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::Read;

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

/// Evaluate a command string for destructive patterns.
/// Returns a DenyReason if the command should be blocked, None if safe.
pub fn evaluate_command(command: &str) -> Option<DenyReason> {
    for segment in split_compound_command(command) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(denial) = evaluate_segment(trimmed) {
            return Some(denial);
        }
    }
    None
}

/// Evaluate a single command segment (no `&&`, `||`, `;` delimiters).
fn evaluate_segment(segment: &str) -> Option<DenyReason> {
    check_git_force_push(segment)
        .or_else(|| check_git_reset_hard(segment))
        .or_else(|| check_git_clean_force(segment))
        .or_else(|| check_git_checkout_dot(segment))
        .or_else(|| check_rm_rf_root(segment))
}

/// Split a compound command on `&&`, `||`, `;`, and `|` delimiters.
/// Simple split — does not handle quoting. Sufficient for Claude-generated commands.
fn split_compound_command(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut rest = command;

    loop {
        // Find the earliest delimiter. Order matters: check multi-char delimiters
        // before single-char ones so " && " is matched before "&&" could interfere.
        // " | " must come after " || " to avoid partial matches.
        let delimiters: &[&str] = &[" && ", " || ", "; ", " | "];
        let earliest = delimiters
            .iter()
            .filter_map(|d| rest.find(d).map(|pos| (pos, d.len())))
            .min_by_key(|(pos, _)| *pos);

        match earliest {
            Some((pos, len)) => {
                segments.push(&rest[..pos]);
                rest = &rest[pos + len..];
            }
            None => {
                segments.push(rest);
                break;
            }
        }
    }

    segments
}

// ── Destructive Pattern Checks ──────────────────────────

/// Detect `git push --force` or `git push -f`, but NOT `--force-with-lease`.
fn check_git_force_push(segment: &str) -> Option<DenyReason> {
    if !segment.contains("git") || !segment.contains("push") {
        return None;
    }

    let words: Vec<&str> = segment.split_whitespace().collect();

    // Find "git" followed by "push"
    let git_pos = words.iter().position(|w| *w == "git")?;
    let push_pos = words.iter().skip(git_pos + 1).position(|w| *w == "push")?;
    let push_abs = git_pos + 1 + push_pos;

    // Check args after "push" for force flags
    for word in &words[push_abs + 1..] {
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
    if !segment.contains("git") || !segment.contains("reset") {
        return None;
    }

    let words: Vec<&str> = segment.split_whitespace().collect();
    let git_pos = words.iter().position(|w| *w == "git")?;
    let reset_pos = words.iter().skip(git_pos + 1).position(|w| *w == "reset")?;
    let reset_abs = git_pos + 1 + reset_pos;

    for word in &words[reset_abs + 1..] {
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
    if !segment.contains("git") || !segment.contains("clean") {
        return None;
    }

    let words: Vec<&str> = segment.split_whitespace().collect();
    let git_pos = words.iter().position(|w| *w == "git")?;
    let clean_pos = words.iter().skip(git_pos + 1).position(|w| *w == "clean")?;
    let clean_abs = git_pos + 1 + clean_pos;

    // Collect all short-flag characters across all flag arguments after "clean".
    // This handles both combined (-fd) and separate (-f -d) flag styles.
    let mut flag_chars = String::new();
    for word in &words[clean_abs + 1..] {
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
}
