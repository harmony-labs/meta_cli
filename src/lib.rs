use anyhow::{Result, Context};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
use std::process::Command;
use rayon::prelude::*;
use serde_json;

pub struct LoopConfig {
    pub directories: Vec<String>,
    pub ignore: Vec<String>,
    pub verbose: bool,
    pub silent: bool,
    pub parallel: bool,
}

pub fn run(config: &LoopConfig, command: &str) -> Result<()> {
    let dirs = expand_directories(&config.directories, &config.ignore)?;

    if config.parallel {
        dirs.par_iter().for_each(|dir| {
            run_command(dir, command, config.verbose).unwrap();
        });
    } else {
        for dir in dirs {
            run_command(&dir, command, config.verbose)?;
        }
    }

    Ok(())
}

fn run_command(dir: &PathBuf, command: &str, verbose: bool) -> Result<()> {
    if verbose {
        println!("Executing in directory: {}", dir.display());
    }

    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(dir)
        .output()?;

    if !output.status.success() {
        anyhow::bail!("Command failed in directory: {}", dir.display());
    }

    Ok(())
}

fn expand_directories(directories: &[String], ignore: &[String]) -> Result<Vec<PathBuf>> {
    let mut expanded = Vec::new();

    for dir in directories {
        for entry in WalkDir::new(dir).follow_links(true).into_iter().filter_entry(|e| {
            !ignore.iter().any(|i| e.path().to_string_lossy().contains(i))
        }) {
            let entry = entry?;
            if entry.file_type().is_dir() {
                expanded.push(entry.path().to_path_buf());
            }
        }
    }

    Ok(expanded)
}

pub fn parse_config(config_path: &PathBuf) -> Result<LoopConfig> {
    let config_str = std::fs::read_to_string(config_path)?;
    let config: LoopConfig = serde_json::from_str(&config_str)?;
    Ok(config)
}
