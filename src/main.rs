use anyhow::{Context, Result};
use clap::{Parser, CommandFactory};
use colored::*;
use loop_lib::run;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

mod plugins;
use plugins::PluginManager;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(long, help = "Add shell aliases to the global .looprc file")]
    add_aliases_to_global_looprc: bool,

    #[arg(trailing_var_arg = true)]
    command: Vec<String>,

    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    #[arg(short, long, help = "Specify directories to exclude")]
    exclude: Option<Vec<String>>,

    #[arg(short, long, help = "Specify directories to include")]
    include: Option<Vec<String>>,

    #[arg(short, long, help = "Enable silent mode")]
    silent: bool,

    #[arg(short, long, action, help = "Enable verbose output")]
    verbose: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.command.is_empty() {
        Cli::command().print_help()?;
        std::process::exit(0);
    }

    let command_str = cli.command.join(" ");

    let meta_file_path = cli.config.unwrap_or_else(|| PathBuf::from(".meta"));
    let absolute_path = std::env::current_dir()?.join(&meta_file_path);

    if cli.verbose {
        println!("{}", "Verbose mode enabled".green());
        println!("Resolved config file path: {}", absolute_path.display());
        println!("Executing command: {}", command_str);
    }

    let mut plugin_manager = PluginManager::new();
    plugin_manager.load_plugins()?;

    // Check if help is requested
    let help_requested = cli.command.iter().any(|arg| arg == "--help" || arg == "-h");
    if help_requested {
        if let Some((mode, plugin_help)) = plugin_manager.get_plugin_help_output(&cli.command) {
            match mode {
                meta_plugin_api::HelpMode::Override => {
                    println!("{}", plugin_help);
                    std::process::exit(0);
                }
                meta_plugin_api::HelpMode::Prepend => {
                    println!("{}", plugin_help);
                    // Fall through to show system help as well
                }
                meta_plugin_api::HelpMode::None => {
                    // Fall through to show system help only
                }
            }
        }
    }

    let (meta_projects, ignore_list) = parse_meta_config(&absolute_path)?;
    let mut projects = vec![".".to_string()];
    projects.extend(meta_projects);

    // Parse CLI filtering options
    let mut include_filters: Vec<String> = vec![];
    let mut exclude_filters: Vec<String> = vec![];
    let mut cleaned_command = vec![];

    let mut idx = 0;
    while idx < cli.command.len() {
        match cli.command[idx].as_str() {
            "--include-only" => {
                idx += 1;
                while idx < cli.command.len() && !cli.command[idx].starts_with("--") {
                    let parts = cli.command[idx]
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    include_filters.extend(parts);
                    idx += 1;
                }
            }
            "--exclude" => {
                idx += 1;
                while idx < cli.command.len() && !cli.command[idx].starts_with("--") {
                    let parts = cli.command[idx]
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    exclude_filters.extend(parts);
                    idx += 1;
                }
            }
            arg => {
                cleaned_command.push(arg.to_string());
                idx += 1;
            }
        }
    }


    let command_str = cleaned_command.join(" ");

    let config = loop_lib::LoopConfig {
        add_aliases_to_global_looprc: cli.add_aliases_to_global_looprc,
        directories: projects.clone(),
        ignore: ignore_list,
        include_filters: if include_filters.is_empty() { None } else { Some(include_filters) },
        exclude_filters: if exclude_filters.is_empty() { None } else { Some(exclude_filters) },
        verbose: cli.verbose,
        silent: cli.silent,
    };


    let is_git_clone = cli.command.get(0).map(|s| s == "git").unwrap_or(false)
        && cli.command.get(1).map(|s| s == "clone").unwrap_or(false);

    if plugin_manager.dispatch_command(&cli.command, &projects)? {
        if cli.verbose {
            println!("{}", "Command handled by plugin.".green());
        }
    } else if is_git_clone {
        if cli.verbose {
            println!("{}", "No plugin handled git clone, skipping loop fallback.".yellow());
        }
        // Do nothing, plugin already handled or skipped
    } else {
        if cli.verbose {
            println!("{}", "No plugin handled command, falling back to loop.".yellow());
        }
        run(&config, &command_str)?;
    }

    Ok(())
}

fn parse_meta_config(meta_path: &std::path::Path) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let config_str = std::fs::read_to_string(meta_path)
        .with_context(|| format!("Failed to read meta config file: '{}'", meta_path.display()))?;
    let meta_config: serde_json::Value = serde_json::from_str(&config_str)
        .with_context(|| format!("Failed to parse meta config file: {}", meta_path.display()))?;
    let projects = meta_config["projects"].as_object()
        .unwrap_or(&serde_json::Map::new())
        .keys()
        .cloned()
        .collect::<Vec<String>>();
    let ignore = meta_config["ignore"].as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect::<Vec<String>>();
    Ok((projects, ignore))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use std::io::Write;

    #[test]
    fn test_parse_meta_config_valid() {
        let mut file = NamedTempFile::new().unwrap();
        write!(
            file,
            r#"{{
                "projects": {{
                    "repo1": "./repo1",
                    "repo2": "./repo2"
                }},
                "ignore": ["target", "node_modules"]
            }}"#
        )
        .unwrap();

        let (projects, ignore) = parse_meta_config(file.path()).unwrap();
        assert_eq!(projects.len(), 2);
        assert!(projects.contains(&"repo1".to_string()));
        assert!(projects.contains(&"repo2".to_string()));
        assert_eq!(ignore, vec!["target".to_string(), "node_modules".to_string()]);
    }

    #[test]
    fn test_parse_meta_config_missing_keys() {
        let mut file = NamedTempFile::new().unwrap();
        write!(
            file,
            r#"{{
                "not_projects": {{}}
            }}"#
        )
        .unwrap();

        let (projects, ignore) = parse_meta_config(file.path()).unwrap();
        assert!(projects.is_empty());
        assert!(ignore.is_empty());
    }

    #[test]
    fn test_parse_meta_config_invalid_json() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "invalid json").unwrap();

        let result = parse_meta_config(file.path());
        assert!(result.is_err());
    }
}
