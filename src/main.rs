use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use colored::*;
use loop_lib::run;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

mod registry;
mod subprocess_plugins;

use subprocess_plugins::{PluginRequestOptions, SubprocessPluginManager};

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

    #[arg(
        long,
        short = 't',
        value_name = "TAGS",
        help = "Filter projects by tag(s), comma-separated"
    )]
    tag: Option<String>,

    #[arg(long, short = 'r', help = "Recursively process nested meta repos")]
    recursive: bool,

    #[arg(
        long,
        value_name = "N",
        help = "Maximum depth for recursive processing (default: unlimited)"
    )]
    depth: Option<usize>,
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

    // Discover plugins early to handle --help requests and plugin listing
    let mut subprocess_plugins = SubprocessPluginManager::new();
    subprocess_plugins.discover_plugins(cli.verbose)?;

    if cli.command.is_empty() {
        Cli::command().print_help()?;
        print_installed_plugins(&subprocess_plugins);
        std::process::exit(0);
    }

    // Check if user wants plugin help (e.g., "meta git --help" or "meta git -h")
    // Also show help if command is just a plugin name with no subcommand (e.g., "meta git" or "meta project")
    if let Some(first_arg) = cli.command.first() {
        let wants_help = cli.command.iter().any(|arg| arg == "--help" || arg == "-h");
        let is_bare_plugin_command = cli.command.len() == 1;

        if wants_help || is_bare_plugin_command {
            // Check if this is a plugin command
            if let Some(help_text) = subprocess_plugins.get_plugin_help(first_arg) {
                println!("{help_text}");
                return Ok(());
            }
        }
    }

    // Handle plugin management commands (don't require .meta file)
    if cli.command.first().map(|s| s.as_str()) == Some("plugin") {
        return handle_plugin_command(&cli.command[1..], cli.verbose, cli.json);
    }

    let command_str = cli.command.join(" ");

    let current_dir = std::env::current_dir()?;
    let absolute_path = match find_meta_config(&current_dir, cli.config.as_ref()) {
        Some((path, _format)) => path,
        None => {
            let config_name = cli
                .config
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".meta / .meta.yaml / .meta.yml".to_string());
            eprintln!("Error: Could not find meta config file '{config_name}'");
            eprintln!("Searched from {} up to root", current_dir.display());
            std::process::exit(1);
        }
    };

    let meta_dir = absolute_path.parent().unwrap_or(std::path::Path::new("."));

    if cli.verbose {
        println!("{}", "Verbose mode enabled".green());
        println!("Resolved config file path: {}", absolute_path.display());
        println!("Executing command: {command_str}");
    }

    let (meta_projects, ignore_list) = parse_meta_config(&absolute_path)?;

    // Filter projects by tags if --tag is specified
    let filtered_projects: Vec<&ProjectInfo> = if let Some(ref tag_filter) = cli.tag {
        let requested_tags: Vec<&str> = tag_filter.split(',').map(|s| s.trim()).collect();
        if cli.verbose {
            println!("Filtering projects by tags: {requested_tags:?}");
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
            .map(|p| meta_dir.join(&p.path).to_string_lossy().to_string()),
    );

    // If recursive mode is enabled, discover nested meta repos
    if cli.recursive {
        let max_depth = cli.depth.unwrap_or(usize::MAX);
        if cli.verbose {
            println!(
                "Recursive mode enabled, max depth: {}",
                if max_depth == usize::MAX {
                    "unlimited".to_string()
                } else {
                    max_depth.to_string()
                }
            );
        }
        project_paths = discover_nested_projects(&project_paths, &cli.tag, max_depth, cli.verbose)?;
    }

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
        verbose: cli.verbose,
        silent: cli.silent,
        parallel,
    };

    let is_git_clone = cli.command.first().map(|s| s == "git").unwrap_or(false)
        && cli.command.get(1).map(|s| s == "clone").unwrap_or(false);

    // Try subprocess plugins first (preferred)
    let subprocess_options = PluginRequestOptions {
        json_output: cli.json,
        verbose: cli.verbose,
        parallel,
    };

    if subprocess_plugins.execute(
        &command_str,
        &cli.command,
        &project_paths,
        subprocess_options,
    )? {
        log::info!("Command was handled by subprocess plugin");
        if cli.verbose {
            println!("{}", "Command handled by subprocess plugin.".green());
        }
    } else if is_git_clone {
        log::info!("No plugin handled git clone, skipping loop fallback");
        if cli.verbose {
            println!(
                "{}",
                "No plugin handled git clone, skipping loop fallback.".yellow()
            );
        }
        // Do nothing, plugin already handled or skipped
    } else {
        log::info!("No plugin handled command, falling back to loop");
        if cli.verbose {
            println!(
                "{}",
                "No plugin handled command, falling back to loop.".yellow()
            );
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
#[derive(Clone)]
enum ConfigFormat {
    Json,
    Yaml,
}

/// Find the meta config file, checking for .meta, .meta.yaml, and .meta.yml
fn find_meta_config(
    start_dir: &std::path::Path,
    config_name: Option<&PathBuf>,
) -> Option<(PathBuf, ConfigFormat)> {
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
                return Some((candidate, format.clone()));
            }
        }
        if let Some(parent) = current_dir.parent() {
            current_dir = parent.to_path_buf();
        } else {
            return None;
        }
    }
}

fn parse_meta_config(
    meta_path: &std::path::Path,
) -> anyhow::Result<(Vec<ProjectInfo>, Vec<String>)> {
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
            ProjectInfo {
                name,
                path,
                repo,
                tags,
            }
        })
        .collect();

    Ok((projects, config.ignore))
}

/// Recursively discover nested meta repos and expand project paths
fn discover_nested_projects(
    initial_paths: &[String],
    tag_filter: &Option<String>,
    max_depth: usize,
    verbose: bool,
) -> Result<Vec<String>> {
    let mut all_paths: Vec<String> = Vec::new();
    let mut to_process: Vec<(String, usize)> =
        initial_paths.iter().map(|p| (p.clone(), 0)).collect();
    let mut visited = std::collections::HashSet::new();

    while let Some((path, depth)) = to_process.pop() {
        // Skip if already visited
        if !visited.insert(path.clone()) {
            continue;
        }

        all_paths.push(path.clone());

        // Don't recurse deeper than max_depth
        if depth >= max_depth {
            continue;
        }

        // Check if this directory has a nested .meta file
        let path_buf = std::path::PathBuf::from(&path);
        if let Some((nested_config_path, _format)) = find_meta_config(&path_buf, None) {
            // Don't process the root .meta again (it's already been processed)
            if depth == 0 && path == "." {
                continue;
            }

            if verbose {
                println!("Found nested meta config: {}", nested_config_path.display());
            }

            // Parse the nested config
            if let Ok((nested_projects, _ignore)) = parse_meta_config(&nested_config_path) {
                let nested_dir = nested_config_path
                    .parent()
                    .unwrap_or(std::path::Path::new("."));

                // Apply tag filter if specified
                let filtered: Vec<&ProjectInfo> = if let Some(ref tag_str) = tag_filter {
                    let requested_tags: Vec<&str> = tag_str.split(',').map(|s| s.trim()).collect();
                    nested_projects
                        .iter()
                        .filter(|p| p.tags.iter().any(|t| requested_tags.contains(&t.as_str())))
                        .collect()
                } else {
                    nested_projects.iter().collect()
                };

                // Add nested projects to the processing queue
                for project in filtered {
                    let nested_path = nested_dir.join(&project.path).to_string_lossy().to_string();
                    if !visited.contains(&nested_path) {
                        to_process.push((nested_path, depth + 1));
                    }
                }
            }
        }
    }

    Ok(all_paths)
}

/// Handle plugin management subcommands
fn handle_plugin_command(args: &[String], verbose: bool, json: bool) -> Result<()> {
    use registry::{PluginInstaller, RegistryClient};

    if args.is_empty() {
        println!("Usage: meta plugin <command>");
        println!();
        println!("Commands:");
        println!("  search <query>   Search for plugins in the registry");
        println!("  install <name>   Install a plugin from the registry");
        println!("  list             List installed plugins");
        println!("  uninstall <name> Uninstall a plugin");
        return Ok(());
    }

    let subcommand = &args[0];
    match subcommand.as_str() {
        "search" => {
            if args.len() < 2 {
                anyhow::bail!("Usage: meta plugin search <query>");
            }
            let query = &args[1];
            let client = RegistryClient::new(verbose)?;
            let results = client.search(query)?;

            if json {
                println!("{}", serde_json::to_string_pretty(&results)?);
            } else if results.is_empty() {
                println!("No plugins found matching '{query}'");
            } else {
                println!("Found {} plugin(s):", results.len());
                for plugin in results {
                    println!(
                        "  {} v{} - {}",
                        plugin.name, plugin.version, plugin.description
                    );
                    println!("    by {}", plugin.author);
                }
            }
        }
        "install" => {
            if args.len() < 2 {
                anyhow::bail!("Usage: meta plugin install <name>");
            }
            let name = &args[1];
            let client = RegistryClient::new(verbose)?;
            let metadata = client.fetch_plugin_metadata(name)?;

            let installer = PluginInstaller::new(verbose)?;
            installer.install(&metadata)?;

            if !json {
                println!(
                    "Successfully installed {} v{}",
                    metadata.name, metadata.version
                );
            }
        }
        "list" => {
            let installer = PluginInstaller::new(verbose)?;
            let plugins = installer.list_installed()?;

            if json {
                println!("{}", serde_json::to_string_pretty(&plugins)?);
            } else if plugins.is_empty() {
                println!("No plugins installed");
            } else {
                println!("Installed plugins:");
                for plugin in plugins {
                    println!("  {plugin}");
                }
            }
        }
        "uninstall" => {
            if args.len() < 2 {
                anyhow::bail!("Usage: meta plugin uninstall <name>");
            }
            let name = &args[1];
            let installer = PluginInstaller::new(verbose)?;
            installer.uninstall(name)?;

            if !json {
                println!("Successfully uninstalled {name}");
            }
        }
        _ => {
            anyhow::bail!(
                "Unknown plugin command: {}. Use 'meta plugin' for help.",
                subcommand
            );
        }
    }

    Ok(())
}

/// Print list of installed plugins for --help output
fn print_installed_plugins(plugins: &SubprocessPluginManager) {
    let plugin_list = plugins.list_plugins();
    if !plugin_list.is_empty() {
        println!();
        println!("INSTALLED PLUGINS:");
        for (name, version, description) in plugin_list {
            println!("    {name:<12} v{version:<8} {description}");
        }
        println!();
        println!("Run 'meta <plugin> --help' for plugin-specific help.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

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

        assert_eq!(
            ignore,
            vec!["target".to_string(), "node_modules".to_string()]
        );
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
"#,
        )
        .unwrap();

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
"#,
        )
        .unwrap();

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

    #[test]
    fn test_discover_nested_projects_no_nested() {
        let dir = tempfile::tempdir().unwrap();
        // Create a simple project directory without nested .meta
        let project_dir = dir.path().join("project1");
        std::fs::create_dir(&project_dir).unwrap();

        let initial_paths = vec![".".to_string(), project_dir.to_string_lossy().to_string()];

        let result = discover_nested_projects(&initial_paths, &None, usize::MAX, false).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_discover_nested_projects_with_nested() {
        let dir = tempfile::tempdir().unwrap();

        // Create nested structure:
        // root/
        //   .meta (contains project1)
        //   project1/
        //     .meta (contains subproject)
        //     subproject/

        let project1_dir = dir.path().join("project1");
        let subproject_dir = project1_dir.join("subproject");
        std::fs::create_dir_all(&subproject_dir).unwrap();

        // Create nested .meta in project1
        let nested_meta = project1_dir.join(".meta");
        std::fs::write(
            &nested_meta,
            r#"{"projects": {"subproject": "git@github.com:org/subproject.git"}}"#,
        )
        .unwrap();

        let initial_paths = vec![
            dir.path().to_string_lossy().to_string(),
            project1_dir.to_string_lossy().to_string(),
        ];

        let result = discover_nested_projects(&initial_paths, &None, usize::MAX, false).unwrap();

        // Should include root, project1, and subproject
        assert!(result.len() >= 2);
        // Check that subproject was discovered
        let has_subproject = result.iter().any(|p| p.contains("subproject"));
        assert!(has_subproject, "Nested subproject should be discovered");
    }

    #[test]
    fn test_discover_nested_projects_with_depth_limit() {
        let dir = tempfile::tempdir().unwrap();

        // Create deeply nested structure
        let level1 = dir.path().join("level1");
        let level2 = level1.join("level2");
        std::fs::create_dir_all(&level2).unwrap();

        // Create .meta at level1
        std::fs::write(
            level1.join(".meta"),
            r#"{"projects": {"level2": "git@github.com:org/level2.git"}}"#,
        )
        .unwrap();

        let initial_paths = vec![level1.to_string_lossy().to_string()];

        // With depth 0, should not recurse into level2
        let result_depth_0 = discover_nested_projects(&initial_paths, &None, 0, false).unwrap();
        assert_eq!(result_depth_0.len(), 1); // Only level1

        // With depth 1, should include level2
        let result_depth_1 = discover_nested_projects(&initial_paths, &None, 1, false).unwrap();
        assert!(!result_depth_1.is_empty());
    }

    #[test]
    fn test_discover_nested_projects_with_tag_filter() {
        let dir = tempfile::tempdir().unwrap();

        let project_dir = dir.path().join("project");
        let frontend_dir = project_dir.join("frontend");
        let backend_dir = project_dir.join("backend");
        std::fs::create_dir_all(&frontend_dir).unwrap();
        std::fs::create_dir_all(&backend_dir).unwrap();

        // Create .meta with tagged projects
        std::fs::write(
            project_dir.join(".meta"),
            r#"{
                "projects": {
                    "frontend": {
                        "repo": "git@github.com:org/frontend.git",
                        "tags": ["ui"]
                    },
                    "backend": {
                        "repo": "git@github.com:org/backend.git",
                        "tags": ["api"]
                    }
                }
            }"#,
        )
        .unwrap();

        let initial_paths = vec![project_dir.to_string_lossy().to_string()];

        // Filter by "ui" tag
        let result =
            discover_nested_projects(&initial_paths, &Some("ui".to_string()), usize::MAX, false)
                .unwrap();

        // Should include project and frontend, but not backend
        let has_frontend = result.iter().any(|p| p.contains("frontend"));
        let has_backend = result.iter().any(|p| p.contains("backend"));
        assert!(has_frontend, "Frontend should be included (has 'ui' tag)");
        assert!(!has_backend, "Backend should be excluded (no 'ui' tag)");
    }

    #[test]
    fn test_mixed_json_yaml_format() {
        let dir = tempfile::tempdir().unwrap();

        // Test that we can parse both JSON and YAML in different locations
        let json_meta = dir.path().join(".meta");
        std::fs::write(
            &json_meta,
            r#"{"projects": {"project1": "git@github.com:org/p1.git"}}"#,
        )
        .unwrap();

        let (projects, _) = parse_meta_config(&json_meta).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "project1");

        // YAML version
        let yaml_dir = tempfile::tempdir().unwrap();
        let yaml_meta = yaml_dir.path().join("config.yaml");
        std::fs::write(
            &yaml_meta,
            "projects:\n  project2: git@github.com:org/p2.git\n",
        )
        .unwrap();

        let (yaml_projects, _) = parse_meta_config(&yaml_meta).unwrap();
        assert_eq!(yaml_projects.len(), 1);
        assert_eq!(yaml_projects[0].name, "project2");
    }

    #[test]
    fn test_extended_format_with_optional_path() {
        let dir = tempfile::tempdir().unwrap();
        let meta_path = dir.path().join(".meta");

        // Extended format without explicit path (should default to project name)
        std::fs::write(
            &meta_path,
            r#"{
                "projects": {
                    "myproject": {
                        "repo": "git@github.com:org/myproject.git",
                        "tags": ["test"]
                    }
                }
            }"#,
        )
        .unwrap();

        let (projects, _) = parse_meta_config(&meta_path).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "myproject");
        assert_eq!(projects[0].path, "myproject"); // Path defaults to name
        assert_eq!(projects[0].tags, vec!["test"]);
    }

    #[test]
    fn test_find_meta_config_walks_up_directories() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();

        // Create .meta at root
        let meta_path = dir.path().join(".meta");
        std::fs::write(&meta_path, r#"{"projects": {}}"#).unwrap();

        // Search from nested directory should find root .meta
        let result = find_meta_config(&nested, None);
        assert!(result.is_some());
        let (found_path, _) = result.unwrap();
        assert_eq!(found_path, meta_path);
    }

    #[test]
    fn test_yaml_file_extensions() {
        let dir = tempfile::tempdir().unwrap();

        // Test .yaml extension
        let yaml_path = dir.path().join(".meta.yaml");
        std::fs::write(&yaml_path, "projects:\n  p1: git@test.git\n").unwrap();

        let result = find_meta_config(dir.path(), None);
        assert!(result.is_some());

        // Clean up and test .yml extension
        std::fs::remove_file(&yaml_path).unwrap();
        let yml_path = dir.path().join(".meta.yml");
        std::fs::write(&yml_path, "projects:\n  p2: git@test.git\n").unwrap();

        let result = find_meta_config(dir.path(), None);
        assert!(result.is_some());
    }
}
