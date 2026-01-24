use anyhow::Result;
use clap::{CommandFactory, Parser};
use colored::*;
use loop_lib::run;
use meta_cli::config::{self, find_meta_config, parse_meta_config, MetaTreeNode, ProjectInfo};
use std::path::PathBuf;

mod init;
mod registry;
mod subprocess_plugins;
mod worktree;

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

    #[arg(long, help = "Show what commands would be run without executing them")]
    dry_run: bool,
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

    // Handle init commands (don't require .meta file)
    if cli.command.first().map(|s| s.as_str()) == Some("init") {
        return init::handle_init_command(&cli.command[1..], cli.verbose);
    }

    // Handle worktree commands (some subcommands don't require .meta file)
    if cli.command.first().map(|s| s.as_str()) == Some("worktree") {
        return worktree::handle_worktree_command(&cli.command[1..], cli.verbose, cli.json);
    }

    let command_str = cli.command.join(" ");

    // Check if this is `git clone` - it doesn't require a .meta file because
    // its purpose is to clone the repo that contains the .meta file
    let is_git_clone_bootstrap = cli.command.first().map(|s| s == "git").unwrap_or(false)
        && cli.command.get(1).map(|s| s == "clone").unwrap_or(false);

    if is_git_clone_bootstrap {
        // Handle git clone directly via plugin without requiring .meta file
        // Build args: everything after "git clone" (i.e., URL and other git options)
        let clone_args: Vec<String> = cli.command.iter().skip(2).cloned().collect();

        let subprocess_options = PluginRequestOptions {
            json_output: cli.json,
            verbose: cli.verbose,
            parallel: false,
            dry_run: cli.dry_run,
            silent: cli.silent,
            recursive: false,
            depth: None,
            include_filters: None,
            exclude_filters: None,
        };

        // Pass "git clone" as command, and the URL/options as args
        if subprocess_plugins.execute("git clone", &clone_args, &[], subprocess_options)? {
            if cli.verbose {
                println!("{}", "Git clone handled by subprocess plugin.".green());
            }
            return Ok(());
        } else {
            eprintln!("Error: No plugin available to handle 'git clone'");
            eprintln!("Make sure meta-git plugin is installed.");
            std::process::exit(1);
        }
    }

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

    // Parse CLI filtering options from command args FIRST
    // Note: When using trailing_var_arg, meta's own flags (--recursive, --dry-run, etc.)
    // may end up in the command args if placed after the command. We need to extract them.
    let mut include_filters: Vec<String> = vec![];
    let mut exclude_filters: Vec<String> = vec![];
    let mut parallel = false;
    let mut recursive_from_args = false;
    let mut dry_run_from_args = false;
    let mut depth_from_args: Option<usize> = None;
    let mut cleaned_command = vec![];

    let mut idx = 0;
    while idx < cli.command.len() {
        match cli.command[idx].as_str() {
            "--include" => {
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
            "--recursive" | "-r" => {
                recursive_from_args = true;
                idx += 1;
            }
            "--dry-run" => {
                dry_run_from_args = true;
                idx += 1;
            }
            "--depth" => {
                idx += 1;
                if idx < cli.command.len() {
                    if let Ok(d) = cli.command[idx].parse::<usize>() {
                        depth_from_args = Some(d);
                    }
                    idx += 1;
                }
            }
            arg => {
                cleaned_command.push(arg.to_string());
                idx += 1;
            }
        }
    }

    // Strip leading "exec" and "--" from the command (they are meta syntax, not part of the user command)
    if cleaned_command.first().map(|s| s.as_str()) == Some("exec") {
        cleaned_command.remove(0);
    }
    if cleaned_command.first().map(|s| s.as_str()) == Some("--") {
        cleaned_command.remove(0);
    }

    if cleaned_command.is_empty() {
        eprintln!("Usage: meta exec -- <command> [args...]");
        std::process::exit(1);
    }

    // Merge clap-level --include/--exclude with trailing-arg-parsed filters
    if let Some(ref clap_includes) = cli.include {
        include_filters.extend(clap_includes.iter().cloned());
    }
    if let Some(ref clap_excludes) = cli.exclude {
        exclude_filters.extend(clap_excludes.iter().cloned());
    }

    // Merge flags parsed from args with those parsed by clap
    let recursive = cli.recursive || recursive_from_args;
    let dry_run = cli.dry_run || dry_run_from_args;
    let depth = depth_from_args.or(cli.depth);

    let mut project_paths = vec![".".to_string()];
    project_paths.extend(
        filtered_projects
            .iter()
            .map(|p| meta_dir.join(&p.path).to_string_lossy().to_string()),
    );

    // If recursive mode is enabled, discover nested meta repos
    if recursive {
        if cli.verbose {
            let depth_str = depth.map_or("unlimited".to_string(), |d| d.to_string());
            println!("Recursive mode enabled, max depth: {}", depth_str);
        }
        let tree = config::walk_meta_tree(&meta_dir, depth)?;
        project_paths = vec![".".to_string()];
        let flat = flatten_with_tag_filter(&tree, &cli.tag);
        project_paths.extend(
            flat.iter()
                .map(|p| meta_dir.join(p).to_string_lossy().to_string()),
        );
    }

    let command_str = cleaned_command.join(" ");

    // Prepare filter options (shared by both LoopConfig and PluginRequestOptions)
    let include_opt = if include_filters.is_empty() {
        None
    } else {
        Some(include_filters)
    };
    let exclude_opt = if exclude_filters.is_empty() {
        None
    } else {
        Some(exclude_filters)
    };

    let config = loop_lib::LoopConfig {
        add_aliases_to_global_looprc: cli.add_aliases_to_global_looprc,
        directories: project_paths.clone(),
        ignore: ignore_list,
        include_filters: include_opt.clone(),
        exclude_filters: exclude_opt.clone(),
        verbose: cli.verbose,
        silent: cli.silent,
        parallel,
        dry_run,
        json_output: cli.json,
        spawn_stagger_ms: 0,
    };

    let is_git_clone = cleaned_command.first().map(|s| s == "git").unwrap_or(false)
        && cleaned_command.get(1).map(|s| s == "clone").unwrap_or(false);

    // Try subprocess plugins first (preferred)
    let subprocess_options = PluginRequestOptions {
        json_output: cli.json,
        verbose: cli.verbose,
        parallel,
        dry_run,
        silent: cli.silent,
        recursive,
        depth,
        include_filters: include_opt,
        exclude_filters: exclude_opt,
    };

    if subprocess_plugins.execute(
        &command_str,
        &cleaned_command,
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


/// Flatten a meta tree into path strings, optionally filtering by tag.
/// If tag_filter is Some, only includes nodes whose tags match (and recurses into them).
fn flatten_with_tag_filter(nodes: &[MetaTreeNode], tag_filter: &Option<String>) -> Vec<String> {
    let mut paths = Vec::new();
    flatten_filtered_inner(nodes, tag_filter, "", &mut paths);
    paths
}

fn flatten_filtered_inner(
    nodes: &[MetaTreeNode],
    tag_filter: &Option<String>,
    prefix: &str,
    paths: &mut Vec<String>,
) {
    for node in nodes {
        let matches = match tag_filter {
            Some(ref tag_str) => {
                let requested: Vec<&str> = tag_str.split(',').map(|s| s.trim()).collect();
                node.info.tags.iter().any(|t| requested.contains(&t.as_str()))
            }
            None => true,
        };

        if matches {
            let full_path = if prefix.is_empty() {
                node.info.path.clone()
            } else {
                format!("{}/{}", prefix, node.info.path)
            };
            paths.push(full_path.clone());
            flatten_filtered_inner(&node.children, tag_filter, &full_path, paths);
        }
    }
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
        // Create a .meta with a project that has no nested .meta
        let project_dir = dir.path().join("project1");
        std::fs::create_dir(&project_dir).unwrap();
        std::fs::write(
            dir.path().join(".meta"),
            r#"{"projects": {"project1": "git@github.com:org/project1.git"}}"#,
        )
        .unwrap();

        let tree = config::walk_meta_tree(dir.path(), None).unwrap();
        let paths = config::flatten_meta_tree(&tree);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], "project1");
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

        std::fs::write(
            dir.path().join(".meta"),
            r#"{"projects": {"project1": "git@github.com:org/project1.git"}}"#,
        )
        .unwrap();
        std::fs::write(
            project1_dir.join(".meta"),
            r#"{"projects": {"subproject": "git@github.com:org/subproject.git"}}"#,
        )
        .unwrap();

        let tree = config::walk_meta_tree(dir.path(), None).unwrap();
        let paths = config::flatten_meta_tree(&tree);

        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"project1".to_string()));
        assert!(paths.contains(&"project1/subproject".to_string()));
    }

    #[test]
    fn test_discover_nested_projects_with_depth_limit() {
        let dir = tempfile::tempdir().unwrap();

        let level1 = dir.path().join("level1");
        let level2 = level1.join("level2");
        std::fs::create_dir_all(&level2).unwrap();

        std::fs::write(
            dir.path().join(".meta"),
            r#"{"projects": {"level1": "git@github.com:org/level1.git"}}"#,
        )
        .unwrap();
        std::fs::write(
            level1.join(".meta"),
            r#"{"projects": {"level2": "git@github.com:org/level2.git"}}"#,
        )
        .unwrap();

        // depth 0: no recursion into nested .meta
        let tree_0 = config::walk_meta_tree(dir.path(), Some(0)).unwrap();
        let paths_0 = config::flatten_meta_tree(&tree_0);
        assert_eq!(paths_0, vec!["level1"]);

        // depth 1: recurse one level
        let tree_1 = config::walk_meta_tree(dir.path(), Some(1)).unwrap();
        let paths_1 = config::flatten_meta_tree(&tree_1);
        assert_eq!(paths_1.len(), 2);
        assert!(paths_1.contains(&"level1/level2".to_string()));
    }

    #[test]
    fn test_discover_nested_projects_with_tag_filter() {
        let dir = tempfile::tempdir().unwrap();

        let frontend_dir = dir.path().join("frontend");
        let backend_dir = dir.path().join("backend");
        std::fs::create_dir_all(&frontend_dir).unwrap();
        std::fs::create_dir_all(&backend_dir).unwrap();

        std::fs::write(
            dir.path().join(".meta"),
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

        let tree = config::walk_meta_tree(dir.path(), None).unwrap();
        let result = flatten_with_tag_filter(&tree, &Some("ui".to_string()));

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
