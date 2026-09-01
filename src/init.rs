//! Initialize Claude Code integration for meta repositories.
//!
//! This module provides the `meta init claude` command which installs
//! Claude Code skill files, rules, and hook configuration into the current
//! project's `.claude/` directory.

use anyhow::{Context, Result};
use colored::*;
use serde_json::{json, Map, Value};
use std::fs;
use std::io;
use std::path::Path;

/// Embedded skill files from the meta repository
const SKILL_META_WORKSPACE: &str = include_str!("../.claude/skills/meta-workspace/SKILL.md");
const SKILL_META_GIT: &str = include_str!("../.claude/skills/meta-git/SKILL.md");
const SKILL_META_EXEC: &str = include_str!("../.claude/skills/meta-exec/SKILL.md");
const SKILL_META_PLUGINS: &str = include_str!("../.claude/skills/meta-plugins/SKILL.md");
const SKILL_META_WORKTREE: &str = include_str!("../.claude/skills/meta-worktree/SKILL.md");
const SKILL_META_SAFETY: &str = include_str!("../.claude/skills/meta-safety/SKILL.md");

/// Embedded rule files (always-loaded, survive compaction)
const RULE_WORKSPACE_DISCIPLINE: &str =
    include_str!("../.claude/rules/meta-workspace-discipline.md");
const RULE_DESTRUCTIVE_COMMANDS: &str =
    include_str!("../.claude/rules/meta-destructive-commands.md");

/// All available skills with their directory names
const SKILLS: &[(&str, &str)] = &[
    ("meta-workspace", SKILL_META_WORKSPACE),
    ("meta-git", SKILL_META_GIT),
    ("meta-exec", SKILL_META_EXEC),
    ("meta-plugins", SKILL_META_PLUGINS),
    ("meta-worktree", SKILL_META_WORKTREE),
    ("meta-safety", SKILL_META_SAFETY),
];

/// All available rules with their filenames
const RULES: &[(&str, &str)] = &[
    ("meta-workspace-discipline.md", RULE_WORKSPACE_DISCIPLINE),
    ("meta-destructive-commands.md", RULE_DESTRUCTIVE_COMMANDS),
];

/// Typed init subcommand, mirroring the clap-parsed structure from main.
pub enum InitCommand {
    /// No subcommand — show help
    None,
    /// Install Claude Code skills, rules, and hooks
    Claude {
        /// Overwrite all existing files including settings
        force: bool,
        /// Update skills and rules only, skip settings (preserves user customizations)
        update: bool,
    },
}

/// Handle the `meta init` subcommand with typed args.
pub fn handle_init_command(command: InitCommand, verbose: bool) -> Result<()> {
    match command {
        InitCommand::None => {
            print_init_help();
            Ok(())
        }
        InitCommand::Claude { force, update } => install_claude_integration(force, update, verbose),
    }
}

fn print_init_help() {
    println!("meta init - Initialize meta integrations");
    println!();
    println!("USAGE:");
    println!("    meta init <command>");
    println!();
    println!("COMMANDS:");
    println!("    claude    Install Claude Code skills, rules, and hooks for this meta repo");
    println!();
    println!("OPTIONS:");
    println!("    -f, --force     Overwrite all existing files including settings");
    println!("    -u, --update    Update skills and rules only, preserve settings");
    println!();
    println!("EXAMPLES:");
    println!("    meta init claude             Install Claude integration");
    println!("    meta init claude --update    Update skills/rules, keep settings");
    println!("    meta init claude --force     Overwrite everything");
}

/// Install Claude Code skills and hook configuration
fn install_claude_integration(force: bool, update: bool, verbose: bool) -> Result<()> {
    let current_dir = std::env::current_dir()?;
    install_claude_integration_to(&current_dir, force, update, verbose)
}

/// Install Claude Code skills and hook configuration into a specific directory
fn install_claude_integration_to(
    target_dir: &Path,
    force: bool,
    update: bool,
    verbose: bool,
) -> Result<()> {
    let claude_dir = target_dir.join(".claude");
    let skills_dir = claude_dir.join("skills");
    let rules_dir = claude_dir.join("rules");

    // Check if this looks like a meta repo
    let has_meta_config = target_dir.join(".meta").exists()
        || target_dir.join(".meta.yaml").exists()
        || target_dir.join(".meta.yml").exists();

    if !has_meta_config {
        println!(
            "{}",
            "Warning: No .meta config found in current directory.".yellow()
        );
        println!("This integration is designed for meta repositories.");
        println!();
    }

    // Create directories
    for dir in [&skills_dir, &rules_dir] {
        if !dir.exists() {
            fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create {}", dir.display()))?;
            if verbose {
                println!("Created {}", dir.display());
            }
        }
    }

    let mut installed = 0;
    let mut skipped = 0;
    let mut merged = false;

    // Install skill files into the owner directory, then project them into Claude.
    // --force or --update: refresh changed content; default: skip existing content.
    let overwrite_content = force || update;
    for &(skill_name, content) in SKILLS {
        let owner_dir = target_dir.join(".agents").join("skills").join(skill_name);
        if !owner_dir.exists() {
            fs::create_dir_all(&owner_dir)
                .with_context(|| format!("Failed to create {}", owner_dir.display()))?;
            if verbose {
                println!("Created {}", owner_dir.display());
            }
        }

        let owner_skill_path = owner_dir.join("SKILL.md");
        if install_skill_file(&owner_skill_path, content, overwrite_content, verbose)? {
            installed += 1;
        } else {
            skipped += 1;
        }

        remove_legacy_skill(&skills_dir.join(format!("{skill_name}.md")), verbose)?;

        let harness_skill_path = skills_dir.join(skill_name);
        let relative_owner_path = Path::new("../../.agents/skills").join(skill_name);
        if install_harness_skill(
            &owner_dir,
            &harness_skill_path,
            &relative_owner_path,
            content,
            overwrite_content,
            verbose,
        )? {
            installed += 1;
        }
    }

    // Install rule files
    // --force or --update: overwrite; default: skip existing
    for (filename, content) in RULES {
        let target_path = rules_dir.join(filename);

        if target_path.exists() && !overwrite_content {
            if verbose {
                println!("{} {} (already exists)", "Skipped".yellow(), filename);
            }
            skipped += 1;
            continue;
        }

        write_file(&target_path, content, verbose)?;
        installed += 1;
    }

    // Install/merge settings.json with hook configuration
    // --update: skip entirely (preserve user settings)
    // --force: overwrite completely
    // default: merge meta hooks into existing settings
    let settings_path = claude_dir.join("settings.json");
    if update {
        if verbose {
            println!(
                "{} settings.json (--update preserves settings)",
                "Skipped".yellow()
            );
        }
        skipped += 1;
    } else {
        let result = install_settings(&settings_path, force, verbose)?;
        match result {
            SettingsResult::Created => installed += 1,
            SettingsResult::Overwritten => installed += 1,
            SettingsResult::Merged => {
                merged = true;
                installed += 1;
            }
        }
    }

    // Print summary
    println!();
    if installed > 0 {
        println!(
            "{} Installed {} file(s) to .claude/",
            "✓".green(),
            installed
        );
    }
    if merged {
        println!(
            "{} Merged meta hooks into existing settings.json",
            "✓".green()
        );
    }
    if skipped > 0 {
        let hint = if update {
            "(--update preserves settings)"
        } else {
            "(use --force to overwrite, --update to refresh content)"
        };
        println!(
            "{} Skipped {} existing file(s) {}",
            "•".yellow(),
            skipped,
            hint
        );
    }

    if installed > 0 || merged {
        println!();
        println!("Claude Code is now configured for this meta repository:");
        println!("  Skills:  .claude/skills/ ({} skill files)", SKILLS.len());
        println!("  Rules:   .claude/rules/ ({} rule files)", RULES.len());
        println!("  Hooks:   .claude/settings.json (SessionStart, PreToolUse, PreCompact)");
        println!("  Note:    PreToolUse hooks require the standalone agent CLI (`agent guard`).");
    }

    #[cfg(not(test))]
    if (installed > 0 || merged) && !agent_cli_available() {
        println!(
            "{}",
            "Warning: 'agent' CLI not found. PreToolUse hooks require the standalone agent tool."
                .yellow()
        );
        println!("Install from: https://github.com/gitkb/agent");
    }

    // Try to register GitKB marketplace if claude CLI is available
    // Skip in tests to avoid external CLI calls that slow down test execution
    #[cfg(not(test))]
    register_marketplace(verbose);

    Ok(())
}

/// Result of settings installation
enum SettingsResult {
    Created,
    Overwritten,
    Merged,
}

/// Install or merge settings.json
fn install_settings(settings_path: &Path, force: bool, verbose: bool) -> Result<SettingsResult> {
    let meta_hooks = build_meta_hooks();

    if !settings_path.exists() {
        // Fresh install: create new settings with meta hooks
        let settings = json!({ "hooks": meta_hooks });
        let content = serde_json::to_string_pretty(&settings)?;
        write_file(settings_path, &content, verbose)?;
        return Ok(SettingsResult::Created);
    }

    if force {
        // Force: overwrite completely
        let settings = json!({ "hooks": meta_hooks });
        let content = serde_json::to_string_pretty(&settings)?;
        write_file(settings_path, &content, verbose)?;
        return Ok(SettingsResult::Overwritten);
    }

    // Merge: read existing, deep-merge hooks, write back
    let existing_content = fs::read_to_string(settings_path)
        .with_context(|| format!("Failed to read {}", settings_path.display()))?;

    let existing: Value = serde_json::from_str(&existing_content)
        .with_context(|| format!("Failed to parse {}", settings_path.display()))?;

    let merged = merge_hooks_into_settings(existing, meta_hooks);
    let content = serde_json::to_string_pretty(&merged)?;
    fs::write(settings_path, &content)
        .with_context(|| format!("Failed to write {}", settings_path.display()))?;

    if verbose {
        println!("{} {} (merged)", "Wrote".green(), settings_path.display());
    } else {
        println!("  {} settings.json (merged)", "✓".green());
    }

    Ok(SettingsResult::Merged)
}

/// Build the meta hooks configuration as a JSON object
fn build_meta_hooks() -> Map<String, Value> {
    let mut hooks = Map::new();

    // SessionStart: inject workspace context at session start and after compaction
    hooks.insert(
        "SessionStart".to_string(),
        json!([{
            "hooks": [{
                "type": "command",
                "command": "meta context 2>/dev/null",
                "timeout": 10
            }]
        }]),
    );

    // PreToolUse: block destructive Bash commands
    hooks.insert(
        "PreToolUse".to_string(),
        json!([{
            "matcher": "Bash",
            "hooks": [{
                "type": "command",
                "command": "agent guard",
                "timeout": 5
            }]
        }]),
    );

    // PreCompact: capture workspace state before context compaction
    hooks.insert(
        "PreCompact".to_string(),
        json!([{
            "hooks": [{
                "type": "command",
                "command": "meta context 2>/dev/null",
                "timeout": 10
            }]
        }]),
    );

    hooks
}

/// Merge meta hooks into existing settings, preserving other settings
fn merge_hooks_into_settings(mut existing: Value, meta_hooks: Map<String, Value>) -> Value {
    // Ensure existing has a hooks object
    if !existing.is_object() {
        return json!({ "hooks": meta_hooks });
    }

    let obj = existing.as_object_mut().unwrap();

    // Get or create the hooks object
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut();

    if let Some(hooks) = hooks {
        // For each lifecycle key in meta_hooks, append to existing array
        for (lifecycle, meta_hook_array) in meta_hooks {
            if let Some(existing_array) = hooks.get_mut(&lifecycle) {
                // Existing array: append meta hooks
                if let Some(arr) = existing_array.as_array_mut() {
                    if let Some(meta_arr) = meta_hook_array.as_array() {
                        for hook in meta_arr {
                            // Avoid duplicates by checking if hook already exists
                            if !arr.iter().any(|h| hooks_equal(h, hook)) {
                                arr.push(hook.clone());
                            }
                        }
                    }
                }
            } else {
                // No existing array: add meta hooks
                hooks.insert(lifecycle, meta_hook_array);
            }
        }
    } else {
        // hooks is not an object, replace it
        obj.insert("hooks".to_string(), json!(meta_hooks));
    }

    existing
}

/// Check if two hook entries are effectively equal (same type and command/prompt)
fn hooks_equal(a: &Value, b: &Value) -> bool {
    // Compare the hooks array within each group
    let a_hooks = a.get("hooks").and_then(|h| h.as_array());
    let b_hooks = b.get("hooks").and_then(|h| h.as_array());

    match (a_hooks, b_hooks) {
        (Some(a_arr), Some(b_arr)) => {
            if a_arr.len() != b_arr.len() {
                return false;
            }
            a_arr.iter().zip(b_arr.iter()).all(|(a_hook, b_hook)| {
                let same_type = a_hook.get("type") == b_hook.get("type");
                let same_command = a_hook.get("command") == b_hook.get("command");
                let same_prompt = a_hook.get("prompt") == b_hook.get("prompt");
                same_type && same_command && same_prompt
            })
        }
        _ => false,
    }
}

#[cfg(not(test))]
fn agent_cli_available() -> bool {
    use std::process::{Command, Stdio};

    Command::new("agent")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Register the GitKB marketplace with Claude Code (if available).
/// This is best-effort — if `claude` is not on PATH, skip silently.
#[cfg(not(test))]
fn register_marketplace(verbose: bool) {
    use std::process::Command;

    // Check if claude CLI is available
    let claude_available = Command::new("claude")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !claude_available {
        if verbose {
            println!(
                "{} Claude CLI not found, skipping marketplace registration",
                "•".yellow()
            );
        }
        return;
    }

    // Register the marketplace (idempotent)
    let result = Command::new("claude")
        .args(["plugin", "marketplace", "add", "gitkb/claude-plugins"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match result {
        Ok(status) if status.success() => {
            println!();
            println!("{} Registered GitKB plugin marketplace", "✓".green());
            println!(
                "  Run {} for global plugin access",
                "claude plugin install meta@gitkb".cyan()
            );
        }
        _ => {
            if verbose {
                println!(
                    "{} Failed to register marketplace (non-fatal)",
                    "•".yellow()
                );
            }
        }
    }
}

/// Write a skill file when it is new or its content needs refreshing.
fn install_skill_file(
    path: &Path,
    content: &str,
    overwrite_content: bool,
    verbose: bool,
) -> Result<bool> {
    if path.exists() {
        if !overwrite_content {
            if verbose {
                println!("{} {} (already exists)", "Skipped".yellow(), path.display());
            }
            return Ok(false);
        }

        let existing = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        if existing == content {
            if verbose {
                println!(
                    "{} {} (already up to date)",
                    "Skipped".yellow(),
                    path.display()
                );
            }
            return Ok(false);
        }
    }

    write_file(path, content, verbose)?;
    Ok(true)
}

/// Remove the flat skill file layout previously owned by meta.
fn remove_legacy_skill(path: &Path, verbose: bool) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to inspect {}", path.display()))
        }
    };

    if !metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
        return Ok(());
    }

    fs::remove_file(path).with_context(|| format!("Failed to remove {}", path.display()))?;
    if verbose {
        println!("Removed legacy {}", path.display());
    }

    Ok(())
}

/// Project an owner skill into Claude's native skills directory.
///
/// A directory symlink keeps the harness projection in sync with the canonical
/// `.agents/skills` copy. Platforms that cannot create the link receive the
/// same folder layout directly in the harness directory instead.
fn install_harness_skill(
    owner_dir: &Path,
    harness_path: &Path,
    relative_owner_path: &Path,
    content: &str,
    overwrite_content: bool,
    verbose: bool,
) -> Result<bool> {
    match fs::symlink_metadata(harness_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if symlink_resolves_to(harness_path, owner_dir) {
                return Ok(false);
            }

            fs::remove_file(harness_path)
                .with_context(|| format!("Failed to replace {}", harness_path.display()))?;
        }
        Ok(metadata) if metadata.is_dir() => {
            return install_skill_file(
                &harness_path.join("SKILL.md"),
                content,
                overwrite_content,
                verbose,
            );
        }
        Ok(_) => {
            fs::remove_file(harness_path)
                .with_context(|| format!("Failed to replace {}", harness_path.display()))?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Failed to inspect {}", harness_path.display()))
        }
    }

    match create_directory_symlink(relative_owner_path, harness_path) {
        Ok(()) => {
            if verbose {
                println!(
                    "Linked {} -> {}",
                    harness_path.display(),
                    relative_owner_path.display()
                );
            }
            Ok(false)
        }
        Err(error) => {
            if verbose {
                println!(
                    "Could not link {} ({}); installing a folder instead",
                    harness_path.display(),
                    error
                );
            }
            fs::create_dir_all(harness_path)
                .with_context(|| format!("Failed to create {}", harness_path.display()))?;
            install_skill_file(
                &harness_path.join("SKILL.md"),
                content,
                overwrite_content,
                verbose,
            )
        }
    }
}

fn symlink_resolves_to(path: &Path, owner_dir: &Path) -> bool {
    match (fs::canonicalize(path), fs::canonicalize(owner_dir)) {
        (Ok(target), Ok(owner)) => target == owner,
        _ => false,
    }
}

#[cfg(unix)]
fn create_directory_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_directory_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(not(any(unix, windows)))]
fn create_directory_symlink(_target: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "directory symlinks are not supported on this platform",
    ))
}

fn write_file(path: &Path, content: &str, verbose: bool) -> Result<()> {
    fs::write(path, content).with_context(|| format!("Failed to write {}", path.display()))?;

    if verbose {
        println!("{} {}", "Wrote".green(), path.display());
    } else {
        println!(
            "  {} {}",
            "✓".green(),
            path.file_name().unwrap_or_default().to_string_lossy()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use tempfile::tempdir;

    fn assert_skill_layout(target_dir: &Path, skill_name: &str, expected_content: &str) {
        let owner_dir = target_dir.join(".agents").join("skills").join(skill_name);
        let owner_skill = owner_dir.join("SKILL.md");
        assert!(
            owner_skill.is_file(),
            "{} should exist",
            owner_skill.display()
        );
        assert_eq!(fs::read_to_string(&owner_skill).unwrap(), expected_content);
        assert!(
            expected_content.starts_with(&format!("---\nname: {skill_name}\ndescription:")),
            "{skill_name} should include trigger frontmatter"
        );

        let harness_skill = target_dir.join(".claude").join("skills").join(skill_name);
        let metadata = fs::symlink_metadata(&harness_skill).unwrap();
        if metadata.file_type().is_symlink() {
            assert_eq!(
                fs::read_link(&harness_skill).unwrap(),
                Path::new("../../.agents/skills").join(skill_name)
            );
            assert_eq!(
                fs::canonicalize(&harness_skill).unwrap(),
                fs::canonicalize(&owner_dir).unwrap()
            );
        } else {
            assert!(
                metadata.is_dir(),
                "{} should be a directory",
                harness_skill.display()
            );
            assert_eq!(
                fs::read_to_string(harness_skill.join("SKILL.md")).unwrap(),
                expected_content
            );
        }
    }

    #[test]
    fn test_install_creates_skills_rules_and_settings() {
        let dir = tempdir().unwrap();

        // Create a .meta file so it looks like a meta repo
        fs::write(dir.path().join(".meta"), r#"{"projects": {}}"#).unwrap();

        install_claude_integration_to(dir.path(), false, false, false).unwrap();

        let claude_dir = dir.path().join(".claude");
        let skills_dir = claude_dir.join("skills");
        let rules_dir = claude_dir.join("rules");

        // Canonical skill folders and Claude projections should exist.
        assert!(skills_dir.exists());
        for &(skill_name, content) in SKILLS {
            assert_skill_layout(dir.path(), skill_name, content);
        }

        #[cfg(unix)]
        for &(skill_name, _) in SKILLS {
            assert!(
                fs::symlink_metadata(skills_dir.join(skill_name))
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "{skill_name} should be a symlink on Unix"
            );
        }

        // Rules should exist
        assert!(rules_dir.exists());
        assert!(rules_dir.join("meta-workspace-discipline.md").exists());
        assert!(rules_dir.join("meta-destructive-commands.md").exists());

        // Settings.json should exist with hook configuration
        let settings_path = claude_dir.join("settings.json");
        assert!(settings_path.exists());
        let settings_content = fs::read_to_string(&settings_path).unwrap();
        assert!(settings_content.contains("hooks"));
        assert!(settings_content.contains("SessionStart"));
        assert!(settings_content.contains("PreToolUse"));
        assert!(settings_content.contains("PreCompact"));
        assert!(settings_content.contains("meta context"));
        assert!(settings_content.contains("agent guard"));
    }

    #[test]
    fn test_install_skips_existing_skill_files() {
        let dir = tempdir().unwrap();

        let claude_dir = dir.path().join(".claude");
        let skills_dir = claude_dir.join("skills");
        let rules_dir = claude_dir.join("rules");
        let owner_dir = dir
            .path()
            .join(".agents")
            .join("skills")
            .join("meta-workspace");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::create_dir_all(&rules_dir).unwrap();
        fs::create_dir_all(&owner_dir).unwrap();

        // Create existing canonical skill file.
        let existing_skill = owner_dir.join("SKILL.md");
        fs::write(&existing_skill, "custom content").unwrap();

        // Create existing rule file
        let existing_rule = rules_dir.join("meta-workspace-discipline.md");
        fs::write(&existing_rule, "custom rule").unwrap();

        // Create existing settings.json
        let existing_settings = claude_dir.join("settings.json");
        fs::write(&existing_settings, r#"{"custom": true}"#).unwrap();

        install_claude_integration_to(dir.path(), false, false, false).unwrap();

        // Should not overwrite canonical skill or rule.
        let skill_content = fs::read_to_string(&existing_skill).unwrap();
        assert_eq!(skill_content, "custom content");
        assert_eq!(
            fs::read_to_string(skills_dir.join("meta-workspace").join("SKILL.md")).unwrap(),
            "custom content"
        );

        let rule_content = fs::read_to_string(&existing_rule).unwrap();
        assert_eq!(rule_content, "custom rule");

        // Settings should be MERGED (not overwritten, not skipped)
        let settings_content = fs::read_to_string(&existing_settings).unwrap();
        let settings: Value = serde_json::from_str(&settings_content).unwrap();
        assert!(
            settings.get("custom").is_some(),
            "should preserve custom key"
        );
        assert!(settings.get("hooks").is_some(), "should add hooks");
    }

    #[test]
    fn test_install_force_overwrites() {
        let dir = tempdir().unwrap();

        let claude_dir = dir.path().join(".claude");
        let skills_dir = claude_dir.join("skills");
        let rules_dir = claude_dir.join("rules");
        let owner_dir = dir
            .path()
            .join(".agents")
            .join("skills")
            .join("meta-workspace");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::create_dir_all(&rules_dir).unwrap();
        fs::create_dir_all(&owner_dir).unwrap();

        // Create existing files with different content
        let existing_skill = owner_dir.join("SKILL.md");
        fs::write(&existing_skill, "custom content").unwrap();

        let existing_rule = rules_dir.join("meta-workspace-discipline.md");
        fs::write(&existing_rule, "custom rule").unwrap();

        let existing_settings = claude_dir.join("settings.json");
        fs::write(&existing_settings, r#"{"custom": true}"#).unwrap();

        install_claude_integration_to(dir.path(), true, false, false).unwrap();

        // Should overwrite with embedded content
        let skill_content = fs::read_to_string(&existing_skill).unwrap();
        assert!(skill_content.contains("Meta Workspace Skill"));
        assert_skill_layout(dir.path(), "meta-workspace", SKILL_META_WORKSPACE);

        let rule_content = fs::read_to_string(&existing_rule).unwrap();
        assert!(rule_content.contains("Meta Workspace Discipline"));

        // Settings should be overwritten (not merged)
        let settings_content = fs::read_to_string(&existing_settings).unwrap();
        let settings: Value = serde_json::from_str(&settings_content).unwrap();
        assert!(
            settings.get("custom").is_none(),
            "should not preserve custom key with --force"
        );
        assert!(settings.get("hooks").is_some(), "should have hooks");
    }

    #[test]
    fn test_update_flag_refreshes_content_skips_settings() {
        let dir = tempdir().unwrap();

        let claude_dir = dir.path().join(".claude");
        let skills_dir = claude_dir.join("skills");
        let rules_dir = claude_dir.join("rules");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::create_dir_all(&rules_dir).unwrap();

        // Create a legacy skill file and existing rule.
        let legacy_skill = skills_dir.join("meta-workspace.md");
        fs::write(&legacy_skill, "old skill content").unwrap();

        let existing_rule = rules_dir.join("meta-workspace-discipline.md");
        fs::write(&existing_rule, "old rule content").unwrap();

        let existing_settings = claude_dir.join("settings.json");
        fs::write(
            &existing_settings,
            r#"{"hooks": {"Stop": [{"hooks": [{"type": "prompt", "prompt": "custom"}]}]}}"#,
        )
        .unwrap();

        // Use --update flag
        install_claude_integration_to(dir.path(), false, true, false).unwrap();

        // Skills and rules should be updated
        let skill_content = fs::read_to_string(
            dir.path()
                .join(".agents")
                .join("skills")
                .join("meta-workspace")
                .join("SKILL.md"),
        )
        .unwrap();
        assert!(
            skill_content.contains("Meta Workspace Skill"),
            "skill should be updated"
        );
        assert!(!legacy_skill.exists(), "legacy skill should be removed");
        assert_skill_layout(dir.path(), "meta-workspace", SKILL_META_WORKSPACE);

        let rule_content = fs::read_to_string(&existing_rule).unwrap();
        assert!(
            rule_content.contains("Meta Workspace Discipline"),
            "rule should be updated"
        );

        // Settings should be UNTOUCHED
        let settings_content = fs::read_to_string(&existing_settings).unwrap();
        assert!(
            settings_content.contains("custom"),
            "settings should be preserved"
        );
        assert!(
            !settings_content.contains("SessionStart"),
            "meta hooks should NOT be added"
        );
    }

    #[test]
    fn test_legacy_skill_migration_is_idempotent_for_all_modes() {
        for (force, update) in [(false, false), (true, false), (false, true), (true, true)] {
            let dir = tempdir().unwrap();
            let skills_dir = dir.path().join(".claude").join("skills");
            fs::create_dir_all(&skills_dir).unwrap();

            for &(skill_name, _) in SKILLS {
                fs::write(
                    skills_dir.join(format!("{skill_name}.md")),
                    "legacy skill content",
                )
                .unwrap();
            }
            let unrelated_skill = skills_dir.join("custom-skill.md");
            fs::write(&unrelated_skill, "leave me alone").unwrap();

            install_claude_integration_to(dir.path(), force, update, false).unwrap();

            for &(skill_name, content) in SKILLS {
                assert!(
                    !skills_dir.join(format!("{skill_name}.md")).exists(),
                    "{skill_name} legacy file should be removed"
                );
                assert_skill_layout(dir.path(), skill_name, content);
            }
            assert_eq!(
                fs::read_to_string(&unrelated_skill).unwrap(),
                "leave me alone"
            );

            #[cfg(unix)]
            let workspace_link_inode = fs::symlink_metadata(skills_dir.join("meta-workspace"))
                .unwrap()
                .ino();

            install_claude_integration_to(dir.path(), force, update, false).unwrap();

            for &(skill_name, content) in SKILLS {
                assert_skill_layout(dir.path(), skill_name, content);
            }
            assert_eq!(
                fs::read_to_string(&unrelated_skill).unwrap(),
                "leave me alone"
            );

            #[cfg(unix)]
            assert_eq!(
                fs::symlink_metadata(skills_dir.join("meta-workspace"))
                    .unwrap()
                    .ino(),
                workspace_link_inode,
                "an already-correct skill symlink should be left in place"
            );
        }
    }

    #[test]
    fn test_merge_preserves_existing_hooks() {
        let existing = json!({
            "hooks": {
                "Stop": [{
                    "hooks": [{
                        "type": "prompt",
                        "prompt": "custom stop hook",
                        "timeout": 30
                    }]
                }]
            },
            "permissions": {
                "allow": ["Bash(git:*)"]
            }
        });

        let meta_hooks = build_meta_hooks();
        let merged = merge_hooks_into_settings(existing, meta_hooks);

        // Should preserve existing Stop hook
        assert!(merged["hooks"]["Stop"].is_array());
        let stop_hooks = merged["hooks"]["Stop"].as_array().unwrap();
        assert!(stop_hooks
            .iter()
            .any(|h| h["hooks"][0]["prompt"] == "custom stop hook"));

        // Should add meta hooks
        assert!(merged["hooks"]["SessionStart"].is_array());
        assert!(merged["hooks"]["PreToolUse"].is_array());
        assert!(merged["hooks"]["PreCompact"].is_array());

        // Should preserve other settings
        assert!(merged["permissions"]["allow"].is_array());
    }

    #[test]
    fn test_merge_avoids_duplicate_hooks() {
        // Create existing settings that already have meta hooks
        let existing = json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{
                        "type": "command",
                        "command": "meta context 2>/dev/null",
                        "timeout": 10
                    }]
                }]
            }
        });

        let meta_hooks = build_meta_hooks();
        let merged = merge_hooks_into_settings(existing, meta_hooks);

        // Should NOT duplicate SessionStart hook
        let session_hooks = merged["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_hooks.len(), 1, "should not duplicate existing hook");

        // Should still add other hooks
        assert!(merged["hooks"]["PreToolUse"].is_array());
        assert!(merged["hooks"]["PreCompact"].is_array());
    }

    #[test]
    fn test_settings_does_not_affect_local_settings() {
        let dir = tempdir().unwrap();

        let claude_dir = dir.path().join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();

        // Create a settings.local.json (personal config)
        let local_settings = claude_dir.join("settings.local.json");
        fs::write(
            &local_settings,
            r#"{"permissions": {"allow": ["Bash(git:*)"]}}"#,
        )
        .unwrap();

        fs::write(dir.path().join(".meta"), r#"{"projects": {}}"#).unwrap();
        install_claude_integration_to(dir.path(), false, false, false).unwrap();

        // settings.local.json should be untouched
        let local_content = fs::read_to_string(&local_settings).unwrap();
        assert!(local_content.contains("permissions"));
        assert!(!local_content.contains("hooks"));
    }

    #[test]
    fn test_build_meta_hooks_structure() {
        let hooks = build_meta_hooks();

        // Should have exactly 3 lifecycle hooks
        assert_eq!(hooks.len(), 3);
        assert!(hooks.contains_key("SessionStart"));
        assert!(hooks.contains_key("PreToolUse"));
        assert!(hooks.contains_key("PreCompact"));

        // SessionStart should call meta context
        let session = &hooks["SessionStart"];
        assert!(session[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("meta context"));

        // PreToolUse should have Bash matcher and call agent guard
        let pre_tool = &hooks["PreToolUse"];
        assert_eq!(pre_tool[0]["matcher"], "Bash");
        assert!(pre_tool[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("agent guard"));

        // PreCompact should call meta context
        let pre_compact = &hooks["PreCompact"];
        assert!(pre_compact[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("meta context"));
    }
}
