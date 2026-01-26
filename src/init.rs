//! Initialize Claude Code skills for meta repositories.
//!
//! This module provides the `meta init claude` command which installs
//! Claude Code skill files into the current project's `.claude/skills/` directory.

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
        InitCommand::Claude { force } => install_claude_skills(force, verbose),
    }
}

fn print_init_help() {
    println!("meta init - Initialize meta integrations");
    println!();
    println!("USAGE:");
    println!("    meta init <command>");
    println!();
    println!("COMMANDS:");
    println!("    claude    Install Claude Code skills for this meta repo");
    println!();
    println!("OPTIONS:");
    println!("    -f, --force    Overwrite existing skill files");
    println!();
    println!("EXAMPLES:");
    println!("    meta init claude           Install Claude skills");
    println!("    meta init claude --force   Overwrite existing skills");
}

/// Install Claude Code skill files into .claude/skills/
fn install_claude_skills(force: bool, verbose: bool) -> Result<()> {
    let current_dir = std::env::current_dir()?;
    install_claude_skills_to(&current_dir, force, verbose)
}

/// Install Claude Code skill files into a specific directory's .claude/skills/
fn install_claude_skills_to(target_dir: &Path, force: bool, verbose: bool) -> Result<()> {
    let skills_dir = target_dir.join(".claude").join("skills");

    // Check if this looks like a meta repo
    let has_meta_config = target_dir.join(".meta").exists()
        || target_dir.join(".meta.yaml").exists()
        || target_dir.join(".meta.yml").exists();

    if !has_meta_config {
        println!(
            "{}",
            "Warning: No .meta config found in current directory.".yellow()
        );
        println!("These skills are designed for meta repositories.");
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

    for (filename, content) in SKILLS {
        let target_path = skills_dir.join(filename);

        if target_path.exists() && !force {
            if verbose {
                println!("{} {} (already exists)", "Skipped".yellow(), filename);
            }
            skipped += 1;
            continue;
        }

        write_skill_file(&target_path, content, verbose)?;
        installed += 1;
    }

    // Print summary
    println!();
    if installed > 0 {
        println!(
            "{} Installed {} skill file(s) to .claude/skills/",
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
        println!("Claude Code will now understand how to use meta commands");
        println!("when working in this repository.");
    }

    Ok(())
}

fn write_skill_file(path: &Path, content: &str, verbose: bool) -> Result<()> {
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
    fn test_install_claude_skills_creates_directory() {
        let dir = tempdir().unwrap();

        // Create a .meta file so it looks like a meta repo
        fs::write(dir.path().join(".meta"), r#"{"projects": {}}"#).unwrap();

        install_claude_skills_to(dir.path(), false, false).unwrap();

        let skills_dir = dir.path().join(".claude").join("skills");
        assert!(skills_dir.exists());
        assert!(skills_dir.join("meta-workspace.md").exists());
        assert!(skills_dir.join("meta-git.md").exists());
        assert!(skills_dir.join("meta-exec.md").exists());
        assert!(skills_dir.join("meta-plugins.md").exists());
    }

    #[test]
    fn test_install_claude_skills_skips_existing() {
        let dir = tempdir().unwrap();

        let skills_dir = dir.path().join(".claude").join("skills");
        fs::create_dir_all(&skills_dir).unwrap();

        // Create an existing file with different content
        let existing = skills_dir.join("meta-workspace.md");
        fs::write(&existing, "custom content").unwrap();

        install_claude_skills_to(dir.path(), false, false).unwrap();

        // Should not overwrite
        let content = fs::read_to_string(&existing).unwrap();
        assert_eq!(content, "custom content");
    }

    #[test]
    fn test_install_claude_skills_force_overwrites() {
        let dir = tempdir().unwrap();

        let skills_dir = dir.path().join(".claude").join("skills");
        fs::create_dir_all(&skills_dir).unwrap();

        // Create an existing file with different content
        let existing = skills_dir.join("meta-workspace.md");
        fs::write(&existing, "custom content").unwrap();

        install_claude_skills_to(dir.path(), true, false).unwrap();

        // Should overwrite with embedded content
        let content = fs::read_to_string(&existing).unwrap();
        assert!(content.contains("Meta Workspace Skill"));
    }
}
