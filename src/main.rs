use anyhow::{Context, Result};
use clap::{Parser, CommandFactory};
use colored::*;
use loop_lib::run;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

mod plugins;
mod subprocess_plugins;

use plugins::PluginOptions;
use plugins::PluginManager;
use subprocess_plugins::{SubprocessPluginManager, PluginRequestOptions};

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

    #[arg(long, help = "Output results in JSON format")]
    json: bool,

    #[arg(short, long, help = "Enable silent mode")]
    silent: bool,

    #[arg(short, long, action, help = "Enable verbose output")]
    verbose: bool,

    #[arg(long, short = 't', value_name = "TAGS", help = "Filter projects by tag(s), comma-separated")]
    tag: Option<String>,
}

fn main() -> Result<()> {
    env_logger::init();

    let cli = Cli::parse();

    // Set environment variable for JSON output mode so plugins can detect it
    if cli.json {
        log::debug!("JSON output mode enabled, setting META_JSON_OUTPUT=1");
        std::env::set_var("META_JSON_OUTPUT", "1");
    }
    log::debug!("cli.json = {}", cli.json);

    if cli.command.is_empty() {
        Cli::command().print_help()?;
        std::process::exit(0);
    }

    let command_str = cli.command.join(" ");

    let current_dir = std::env::current_dir()?;
    let absolute_path = match find_meta_config(&current_dir, cli.config.as_ref()) {
        Some((path, _format)) => path,
        None => {
            let config_name = cli.config.as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".meta / .meta.yaml / .meta.yml".to_string());
            eprintln!("Error: Could not find meta config file '{}'", config_name);
            eprintln!("Searched from {} up to root", current_dir.display());
            std::process::exit(1);
        }
    };

    let meta_dir = absolute_path.parent().unwrap_or(std::path::Path::new("."));

    if cli.verbose {
        println!("{}", "Verbose mode enabled".green());
        println!("Resolved config file path: {}", absolute_path.display());
        println!("Executing command: {}", command_str);
    }

    // Discover subprocess plugins (preferred)
    let mut subprocess_plugins = SubprocessPluginManager::new();
    subprocess_plugins.discover_plugins(cli.verbose)?;

    // Also load legacy dylib plugins for backward compatibility
    let mut plugin_manager = PluginManager::new();
    let plugin_options = PluginOptions { verbose: cli.verbose, json: cli.json };
    plugin_manager.load_plugins(&plugin_options)?;

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

    // Filter projects by tags if --tag is specified
    let filtered_projects: Vec<&ProjectInfo> = if let Some(ref tag_filter) = cli.tag {
        let requested_tags: Vec<&str> = tag_filter.split(',').map(|s| s.trim()).collect();
        if cli.verbose {
            println!("Filtering projects by tags: {:?}", requested_tags);
        }
        meta_projects
            .iter()
            .filter(|p| {
                // Project matches if it has any of the requested tags
                p.tags.iter().any(|t| requested_tags.contains(&t.as_str()))
            })
            .collect()
    } else {
        meta_projects.iter().collect()
    };

    let mut project_paths = vec![".".to_string()];
    project_paths.extend(
        filtered_projects
            .iter()
            .map(|p| meta_dir.join(&p.path).to_string_lossy().to_string())
    );

    // Parse CLI filtering options
    let mut include_filters: Vec<String> = vec![];
    let mut exclude_filters: Vec<String> = vec![];
    let mut parallel = false;
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
            "--parallel" => {
                parallel = true;
                idx += 1;
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
        directories: project_paths.clone(),
        ignore: ignore_list,
        include_filters: if include_filters.is_empty() { None } else { Some(include_filters) },
        exclude_filters: if exclude_filters.is_empty() { None } else { Some(exclude_filters) },
        verbose: cli.verbose,
        silent: cli.silent,
        parallel,
    };


    let is_git_clone = cli.command.get(0).map(|s| s == "git").unwrap_or(false)
        && cli.command.get(1).map(|s| s == "clone").unwrap_or(false);

    // Try subprocess plugins first (preferred)
    let subprocess_options = PluginRequestOptions {
        json_output: cli.json,
        verbose: cli.verbose,
        parallel,
    };

    if subprocess_plugins.execute(&command_str, &cli.command, &project_paths, subprocess_options)? {
        log::info!("Command was handled by subprocess plugin");
        if cli.verbose {
            println!("{}", "Command handled by subprocess plugin.".green());
        }
    } else if plugin_manager.dispatch_command(&cli.command, &project_paths)? {
        // Fall back to legacy dylib plugins
        log::info!("Command was handled by dylib plugin");
        if cli.verbose {
            println!("{}", "Command handled by dylib plugin.".green());
        }
    } else if is_git_clone {
        log::info!("No plugin handled git clone, skipping loop fallback");
        if cli.verbose {
            println!("{}", "No plugin handled git clone, skipping loop fallback.".yellow());
        }
        // Do nothing, plugin already handled or skipped
    } else {
        log::info!("No plugin handled command, falling back to loop");
        if cli.verbose {
            println!("{}", "No plugin handled command, falling back to loop.".yellow());
        }
        run(&config, &command_str)?;
    }

    Ok(())
}

/// Represents a project entry in the .meta config.
/// Can be either a simple git URL string or an extended object with optional fields.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ProjectEntry {
    /// Simple format: just a git URL string
    Simple(String),
    /// Extended format: object with repo, optional path, and optional tags
    Extended {
        repo: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        tags: Vec<String>,
    },
}

/// Parsed project info after normalization
#[derive(Debug, Clone)]
pub struct ProjectInfo {
    pub name: String,
    pub path: String,
    pub repo: String,
    pub tags: Vec<String>,
}

/// The meta configuration file structure
#[derive(Debug, Deserialize, Default)]
struct MetaConfig {
    #[serde(default)]
    projects: HashMap<String, ProjectEntry>,
    #[serde(default)]
    ignore: Vec<String>,
}

/// Determines the format of a config file based on extension
enum ConfigFormat {
    Json,
    Yaml,
}

/// Find the meta config file, checking for .meta, .meta.yaml, and .meta.yml
fn find_meta_config(start_dir: &std::path::Path, config_name: Option<&PathBuf>) -> Option<(PathBuf, ConfigFormat)> {
    let candidates: Vec<(String, ConfigFormat)> = if let Some(name) = config_name {
        // User specified a config file name
        let name_str = name.to_string_lossy().to_string();
        if name_str.ends_with(".yaml") || name_str.ends_with(".yml") {
            vec![(name_str, ConfigFormat::Yaml)]
        } else {
            vec![(name_str, ConfigFormat::Json)]
        }
    } else {
        // Default: check all supported names
        vec![
            (".meta".to_string(), ConfigFormat::Json),
            (".meta.yaml".to_string(), ConfigFormat::Yaml),
            (".meta.yml".to_string(), ConfigFormat::Yaml),
        ]
    };

    let mut current_dir = start_dir.to_path_buf();
    loop {
        for (name, format) in &candidates {
            let candidate = current_dir.join(name);
            if candidate.exists() {
                let format = match *format {
                    ConfigFormat::Json => ConfigFormat::Json,
                    ConfigFormat::Yaml => ConfigFormat::Yaml,
                };
                return Some((candidate, format));
            }
        }
        if let Some(parent) = current_dir.parent() {
            current_dir = parent.to_path_buf();
        } else {
            return None;
        }
    }
}

fn parse_meta_config(meta_path: &std::path::Path) -> anyhow::Result<(Vec<ProjectInfo>, Vec<String>)> {
    let config_str = std::fs::read_to_string(meta_path)
        .with_context(|| format!("Failed to read meta config file: '{}'", meta_path.display()))?;

    // Determine format from file extension
    let path_str = meta_path.to_string_lossy();
    let config: MetaConfig = if path_str.ends_with(".yaml") || path_str.ends_with(".yml") {
        serde_yaml::from_str(&config_str)
            .with_context(|| format!("Failed to parse YAML config file: {}", meta_path.display()))?
    } else {
        serde_json::from_str(&config_str)
            .with_context(|| format!("Failed to parse JSON config file: {}", meta_path.display()))?
    };

    // Convert project entries to normalized ProjectInfo
    let projects: Vec<ProjectInfo> = config
        .projects
        .into_iter()
        .map(|(name, entry)| {
            let (repo, path, tags) = match entry {
                ProjectEntry::Simple(repo) => (repo, name.clone(), vec![]),
                ProjectEntry::Extended { repo, path, tags } => {
                    let resolved_path = path.unwrap_or_else(|| name.clone());
                    (repo, resolved_path, tags)
                }
            };
            ProjectInfo { name, path, repo, tags }
        })
        .collect();

    Ok((projects, config.ignore))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use std::io::Write;

    #[test]
    fn test_parse_meta_config_valid_simple_format() {
        let mut file = NamedTempFile::new().unwrap();
        write!(
            file,
            r#"{{
                "projects": {{
                    "repo1": "git@github.com:org/repo1.git",
                    "repo2": "git@github.com:org/repo2.git"
                }},
                "ignore": ["target", "node_modules"]
            }}"#
        )
        .unwrap();

        let (projects, ignore) = parse_meta_config(file.path()).unwrap();
        assert_eq!(projects.len(), 2);

        let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"repo1"));
        assert!(names.contains(&"repo2"));

        // Simple format: path defaults to project name
        let repo1 = projects.iter().find(|p| p.name == "repo1").unwrap();
        assert_eq!(repo1.path, "repo1");
        assert_eq!(repo1.repo, "git@github.com:org/repo1.git");
        assert!(repo1.tags.is_empty());

        assert_eq!(ignore, vec!["target".to_string(), "node_modules".to_string()]);
    }

    #[test]
    fn test_parse_meta_config_extended_format() {
        let mut file = NamedTempFile::new().unwrap();
        write!(
            file,
            r#"{{
                "projects": {{
                    "api": {{
                        "repo": "git@github.com:org/api.git",
                        "path": "./services/api",
                        "tags": ["backend", "rust"]
                    }}
                }}
            }}"#
        )
        .unwrap();

        let (projects, _ignore) = parse_meta_config(file.path()).unwrap();
        assert_eq!(projects.len(), 1);

        let api = &projects[0];
        assert_eq!(api.name, "api");
        assert_eq!(api.path, "./services/api");
        assert_eq!(api.repo, "git@github.com:org/api.git");
        assert_eq!(api.tags, vec!["backend", "rust"]);
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

    #[test]
    fn test_parse_meta_config_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let yaml_path = dir.path().join("test.yaml");
        std::fs::write(
            &yaml_path,
            r#"
projects:
  web-app: git@github.com:org/web-app.git
  api:
    repo: git@github.com:org/api.git
    path: ./backend/api
    tags:
      - backend
      - python
ignore:
  - node_modules
  - __pycache__
"#
        ).unwrap();

        let (projects, ignore) = parse_meta_config(&yaml_path).unwrap();
        assert_eq!(projects.len(), 2);

        let web_app = projects.iter().find(|p| p.name == "web-app").unwrap();
        assert_eq!(web_app.path, "web-app");
        assert_eq!(web_app.repo, "git@github.com:org/web-app.git");

        let api = projects.iter().find(|p| p.name == "api").unwrap();
        assert_eq!(api.path, "./backend/api");
        assert_eq!(api.tags, vec!["backend", "python"]);

        assert_eq!(ignore, vec!["node_modules", "__pycache__"]);
    }

    #[test]
    fn test_find_meta_config() {
        let dir = tempfile::tempdir().unwrap();
        let meta_path = dir.path().join(".meta");
        std::fs::write(&meta_path, r#"{"projects": {}}"#).unwrap();

        let result = find_meta_config(dir.path(), None);
        assert!(result.is_some());
        let (path, _format) = result.unwrap();
        assert_eq!(path, meta_path);
    }

    #[test]
    fn test_find_meta_config_yaml_priority() {
        let dir = tempfile::tempdir().unwrap();
        // Create .meta first (JSON), then .meta.yaml
        std::fs::write(dir.path().join(".meta"), r#"{"projects": {}}"#).unwrap();

        // .meta should be found first (it's checked before .meta.yaml)
        let result = find_meta_config(dir.path(), None);
        assert!(result.is_some());
        let (path, _format) = result.unwrap();
        assert!(path.ends_with(".meta"));
    }

    #[test]
    fn test_parse_meta_config_with_tags() {
        let dir = tempfile::tempdir().unwrap();
        let yaml_path = dir.path().join("test.yaml");
        std::fs::write(
            &yaml_path,
            r#"
projects:
  frontend:
    repo: git@github.com:org/frontend.git
    tags:
      - ui
      - typescript
  backend:
    repo: git@github.com:org/backend.git
    tags:
      - api
      - rust
  shared:
    repo: git@github.com:org/shared.git
    tags:
      - ui
      - api
"#
        ).unwrap();

        let (projects, _ignore) = parse_meta_config(&yaml_path).unwrap();
        assert_eq!(projects.len(), 3);

        let frontend = projects.iter().find(|p| p.name == "frontend").unwrap();
        assert_eq!(frontend.tags, vec!["ui", "typescript"]);

        let backend = projects.iter().find(|p| p.name == "backend").unwrap();
        assert_eq!(backend.tags, vec!["api", "rust"]);

        let shared = projects.iter().find(|p| p.name == "shared").unwrap();
        assert_eq!(shared.tags, vec!["ui", "api"]);

        // Filter projects by tag "ui"
        let ui_projects: Vec<_> = projects
            .iter()
            .filter(|p| p.tags.contains(&"ui".to_string()))
            .collect();
        assert_eq!(ui_projects.len(), 2);

        // Filter projects by tag "rust"
        let rust_projects: Vec<_> = projects
            .iter()
            .filter(|p| p.tags.contains(&"rust".to_string()))
            .collect();
        assert_eq!(rust_projects.len(), 1);
        assert_eq!(rust_projects[0].name, "backend");
    }
}
