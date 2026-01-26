//! Initialize Claude Code integration for meta repositories.
//!
//! This module provides the `meta init claude` command which installs
//! Claude Code skill files and hook configuration into the current
//! project's `.claude/` directory.

use anyhow::{Context, Result};
use colored::*;
use std::fs;
use std::path::Path;

/// Embedded skill files from the meta repository
const SKILL_META_WORKSPACE: &str = include_str!("../../.claude/skills/meta-workspace.md");
const SKILL_META_GIT: &str = include_str!("../../.claude/skills/meta-git.md");
const SKILL_META_EXEC: &str = include_str!("../../.claude/skills/meta-exec.md");
const SKILL_META_PLUGINS: &str = include_str!("../../.claude/skills/meta-plugins.md");
const SKILL_META_WORKTREE: &str = include_str!("../../.claude/skills/meta-worktree.md");

/// Embedded settings template with hook configuration
const SETTINGS_TEMPLATE: &str = include_str!("../../.claude/settings-template.json");

/// All available skills with their filenames
const SKILLS: &[(&str, &str)] = &[
    ("meta-workspace.md", SKILL_META_WORKSPACE),
    ("meta-git.md", SKILL_META_GIT),
    ("meta-exec.md", SKILL_META_EXEC),
    ("meta-plugins.md", SKILL_META_PLUGINS),
    ("meta-worktree.md", SKILL_META_WORKTREE),
];

/// Typed init subcommand, mirroring the clap-parsed structure from main.
pub enum InitCommand {
    /// No subcommand — show help
    None,
    /// Install Claude Code skills
    Claude { force: bool },
}

/// Handle the `meta init` subcommand with typed args.
pub fn handle_init_command(command: InitCommand, verbose: bool) -> Result<()> {
    match command {
        InitCommand::None => {
            print_init_help();
            Ok(())
        }
        InitCommand::Claude { force } => install_claude_integration(force, verbose),
    }
}

fn print_init_help() {
    println!("meta init - Initialize meta integrations");
    println!();
    println!("USAGE:");
    println!("    meta init <command>");
    println!();
    println!("COMMANDS:");
    println!("    claude    Install Claude Code skills and hooks for this meta repo");
    println!();
    println!("OPTIONS:");
    println!("    -f, --force    Overwrite existing files");
    println!();
    println!("EXAMPLES:");
    println!("    meta init claude           Install Claude integration");
    println!("    meta init claude --force   Overwrite existing files");
}

/// Install Claude Code skills and hook configuration
fn install_claude_integration(force: bool, verbose: bool) -> Result<()> {
    let current_dir = std::env::current_dir()?;
    install_claude_integration_to(&current_dir, force, verbose)
}

/// Install Claude Code skills and hook configuration into a specific directory
fn install_claude_integration_to(target_dir: &Path, force: bool, verbose: bool) -> Result<()> {
    let claude_dir = target_dir.join(".claude");
    let skills_dir = claude_dir.join("skills");

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

    // Create .claude/skills directory if it doesn't exist
    if !skills_dir.exists() {
        fs::create_dir_all(&skills_dir)
            .with_context(|| format!("Failed to create {}", skills_dir.display()))?;
        if verbose {
            println!("Created {}", skills_dir.display());
        }
    }

    let mut installed = 0;
    let mut skipped = 0;

    // Install skill files
    for (filename, content) in SKILLS {
        let target_path = skills_dir.join(filename);

        if target_path.exists() && !force {
            if verbose {
                println!("{} {} (already exists)", "Skipped".yellow(), filename);
            }
            skipped += 1;
            continue;
        }

        write_file(&target_path, content, verbose)?;
        installed += 1;
    }

    // Install settings.json with hook configuration
    let settings_path = claude_dir.join("settings.json");
    if settings_path.exists() && !force {
        if verbose {
            println!(
                "{} settings.json (already exists)",
                "Skipped".yellow()
            );
        }
        skipped += 1;
    } else {
        write_file(&settings_path, SETTINGS_TEMPLATE, verbose)?;
        installed += 1;
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
    if skipped > 0 {
        println!(
            "{} Skipped {} existing file(s) (use --force to overwrite)",
            "•".yellow(),
            skipped
        );
    }

    if installed > 0 {
        println!();
        println!("Claude Code is now configured for this meta repository:");
        println!("  Skills:  .claude/skills/ (5 skill files)");
        println!("  Hooks:   .claude/settings.json (Stop hook for repo validation)");
    }

    Ok(())
}

fn write_file(path: &Path, content: &str, verbose: bool) -> Result<()> {
    fs::write(path, content)
        .with_context(|| format!("Failed to write {}", path.display()))?;

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
    use tempfile::tempdir;

    #[test]
    fn test_install_creates_skills_and_settings() {
        let dir = tempdir().unwrap();

        // Create a .meta file so it looks like a meta repo
        fs::write(dir.path().join(".meta"), r#"{"projects": {}}"#).unwrap();

        install_claude_integration_to(dir.path(), false, false).unwrap();

        let claude_dir = dir.path().join(".claude");
        let skills_dir = claude_dir.join("skills");
        assert!(skills_dir.exists());
        assert!(skills_dir.join("meta-workspace.md").exists());
        assert!(skills_dir.join("meta-git.md").exists());
        assert!(skills_dir.join("meta-exec.md").exists());
        assert!(skills_dir.join("meta-plugins.md").exists());
        assert!(skills_dir.join("meta-worktree.md").exists());

        // Settings.json should exist with hook configuration
        let settings_path = claude_dir.join("settings.json");
        assert!(settings_path.exists());
        let settings_content = fs::read_to_string(&settings_path).unwrap();
        assert!(settings_content.contains("hooks"));
        assert!(settings_content.contains("Stop"));
        assert!(settings_content.contains("prompt"));
    }

    #[test]
    fn test_install_skips_existing_files() {
        let dir = tempdir().unwrap();

        let claude_dir = dir.path().join(".claude");
        let skills_dir = claude_dir.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();

        // Create existing skill file
        let existing_skill = skills_dir.join("meta-workspace.md");
        fs::write(&existing_skill, "custom content").unwrap();

        // Create existing settings.json
        let existing_settings = claude_dir.join("settings.json");
        fs::write(&existing_settings, r#"{"custom": true}"#).unwrap();

        install_claude_integration_to(dir.path(), false, false).unwrap();

        // Should not overwrite either file
        let skill_content = fs::read_to_string(&existing_skill).unwrap();
        assert_eq!(skill_content, "custom content");

        let settings_content = fs::read_to_string(&existing_settings).unwrap();
        assert_eq!(settings_content, r#"{"custom": true}"#);
    }

    #[test]
    fn test_install_force_overwrites() {
        let dir = tempdir().unwrap();

        let claude_dir = dir.path().join(".claude");
        let skills_dir = claude_dir.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();

        // Create existing files with different content
        let existing_skill = skills_dir.join("meta-workspace.md");
        fs::write(&existing_skill, "custom content").unwrap();

        let existing_settings = claude_dir.join("settings.json");
        fs::write(&existing_settings, r#"{"custom": true}"#).unwrap();

        install_claude_integration_to(dir.path(), true, false).unwrap();

        // Should overwrite with embedded content
        let skill_content = fs::read_to_string(&existing_skill).unwrap();
        assert!(skill_content.contains("Meta Workspace Skill"));

        let settings_content = fs::read_to_string(&existing_settings).unwrap();
        assert!(settings_content.contains("hooks"));
        assert!(settings_content.contains("Stop"));
    }

    #[test]
    fn test_settings_template_is_valid_json() {
        let parsed: serde_json::Value =
            serde_json::from_str(SETTINGS_TEMPLATE).expect("settings template must be valid JSON");
        assert!(parsed.get("hooks").is_some(), "must have hooks key");
        assert!(
            parsed["hooks"].get("Stop").is_some(),
            "must have Stop hook"
        );
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
        install_claude_integration_to(dir.path(), false, false).unwrap();

        // settings.local.json should be untouched
        let local_content = fs::read_to_string(&local_settings).unwrap();
        assert!(local_content.contains("permissions"));
        assert!(!local_content.contains("hooks"));
    }
}
