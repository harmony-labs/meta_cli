use anyhow::Result;
use clap::{Args, CommandFactory, Parser, Subcommand};
use colored::*;
use loop_lib::run;
use meta_cli::config::{self, find_meta_config, parse_meta_config, MetaTreeNode, ProjectInfo};
use std::io::Write;
use std::path::PathBuf;

mod init;
mod registry;
mod subprocess_plugins;
mod worktree;

use subprocess_plugins::{PluginRequestOptions, SubprocessPluginManager};

// === CLI Structs ===

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(
        long,
        global = true,
        help = "Add shell aliases to the global .looprc file"
    )]
    add_aliases_to_global_looprc: bool,

    #[arg(short, long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    #[arg(
        short,
        long,
        global = true,
        value_delimiter = ',',
        help = "Specify directories to exclude"
    )]
    exclude: Option<Vec<String>>,

    #[arg(
        short,
        long,
        global = true,
        value_delimiter = ',',
        help = "Specify directories to include"
    )]
    include: Option<Vec<String>>,

    #[arg(long, global = true, help = "Output results in JSON format")]
    json: bool,

    #[arg(short, long, global = true, help = "Enable silent mode")]
    silent: bool,

    #[arg(short, long, action, global = true, help = "Enable verbose output")]
    verbose: bool,

    #[arg(
        long,
        short = 't',
        global = true,
        value_name = "TAGS",
        help = "Filter projects by tag(s), comma-separated"
    )]
    tag: Option<String>,

    #[arg(
        long,
        short = 'r',
        global = true,
        help = "Recursively process nested meta repos"
    )]
    recursive: bool,

    #[arg(
        long,
        global = true,
        value_name = "N",
        help = "Maximum depth for recursive processing (default: unlimited)"
    )]
    depth: Option<usize>,

    #[arg(
        long,
        global = true,
        help = "Show what commands would be run without executing them"
    )]
    dry_run: bool,

    #[arg(long, global = true, help = "Run commands in parallel")]
    parallel: bool,

    #[arg(
        long,
        global = true,
        help = "Use primary checkout paths, overriding worktree context detection"
    )]
    primary: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Execute a command across all repos
    Exec(ExecArgs),
    /// Initialize meta integrations
    Init(InitArgs),
    /// Manage plugins
    Plugin(PluginArgs),
    /// Manage git worktrees across repos
    Worktree(WorktreeArgs),
    #[command(external_subcommand)]
    External(Vec<String>),
}

/// Arguments for `meta exec`
#[derive(Args)]
struct ExecArgs {
    /// Command and arguments to execute (use -- to separate from meta flags)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

/// Arguments for `meta init`
#[derive(Args)]
struct InitArgs {
    #[command(subcommand)]
    command: Option<InitCommands>,
}

#[derive(Subcommand)]
enum InitCommands {
    /// Install Claude Code skills for this meta repo
    Claude {
        /// Overwrite existing skill files
        #[arg(short, long)]
        force: bool,
    },
}

/// Arguments for `meta plugin`
#[derive(Args)]
struct PluginArgs {
    #[command(subcommand)]
    command: Option<PluginCommands>,
}

#[derive(Subcommand)]
enum PluginCommands {
    /// Search for plugins in the registry
    Search {
        /// Search query
        query: String,
    },
    /// Install a plugin from the registry
    Install {
        /// Plugin name
        name: String,
    },
    /// List installed plugins
    List,
    /// Uninstall a plugin
    Uninstall {
        /// Plugin name
        name: String,
    },
}

/// Arguments for `meta worktree`
#[derive(Args)]
struct WorktreeArgs {
    #[command(subcommand)]
    command: Option<worktree::WorktreeCommands>,
}

// === Help Utilities ===

/// Print help text and installed plugins to stdout or stderr.
/// Use `to_stderr: true` for error cases where help is shown due to an invalid command.
fn print_help_with_plugins(plugins: &SubprocessPluginManager, to_stderr: bool) {
    if to_stderr {
        let _ = Cli::command().write_help(&mut std::io::stderr());
        eprint_installed_plugins(plugins);
    } else {
        let _ = Cli::command().print_help();
        print_installed_plugins(plugins);
    }
}

/// Write list of installed plugins to a writer.
fn write_installed_plugins(plugins: &SubprocessPluginManager, w: &mut dyn Write) {
    let plugin_list = plugins.list_plugins();
    if !plugin_list.is_empty() {
        let _ = writeln!(w);
        let _ = writeln!(w, "INSTALLED PLUGINS:");
        for (name, version, description) in plugin_list {
            let _ = writeln!(w, "    {name:<12} v{version:<8} {description}");
        }
        let _ = writeln!(w);
        let _ = writeln!(w, "Run 'meta <plugin> --help' for plugin-specific help.");
    }
}

/// Print list of installed plugins for --help output (stdout)
fn print_installed_plugins(plugins: &SubprocessPluginManager) {
    write_installed_plugins(plugins, &mut std::io::stdout());
}

/// Print list of installed plugins to stderr (for error cases)
fn eprint_installed_plugins(plugins: &SubprocessPluginManager) {
    write_installed_plugins(plugins, &mut std::io::stderr());
}

// === Main Entry Point ===

fn main() -> Result<()> {
    env_logger::init();

    let mut cli = Cli::parse();

    log::debug!("cli.json = {}", cli.json);

    // Discover plugins early to handle --help requests and plugin listing
    let mut subprocess_plugins = SubprocessPluginManager::new();
    subprocess_plugins.discover_plugins(cli.verbose)?;

    // Take command out so we can move subcommand args while still borrowing cli
    let command = cli.command.take();
    match command {
        None => {
            print_help_with_plugins(&subprocess_plugins, false);
            std::process::exit(0);
        }
        Some(Commands::Init(args)) => {
            let cmd = match args.command {
                None => init::InitCommand::None,
                Some(InitCommands::Claude { force }) => init::InitCommand::Claude { force },
            };
            init::handle_init_command(cmd, cli.verbose)
        }
        Some(Commands::Plugin(args)) => handle_plugin_command(args.command, cli.verbose, cli.json),
        Some(Commands::Worktree(args)) => match args.command {
            Some(cmd) => worktree::handle_worktree_command(cmd, cli.verbose, cli.json),
            None => {
                worktree::print_worktree_help();
                Ok(())
            }
        },
        Some(Commands::Exec(args)) => {
            handle_command_dispatch(args.command, &cli, &subprocess_plugins, true)
        }
        Some(Commands::External(args)) => {
            // Check for plugin help or bare plugin command first
            if let Some(first) = args.first() {
                let wants_help = args.iter().any(|a| a == "--help" || a == "-h");
                let is_bare = args.len() == 1;
                if wants_help || is_bare {
                    if let Some(help_text) = subprocess_plugins.get_plugin_help(first) {
                        println!("{help_text}");
                        return Ok(());
                    }
                }
            }
            handle_command_dispatch(args, &cli, &subprocess_plugins, false)
        }
    }
}

// === Command Dispatch (shared by exec and external) ===

/// Dispatch a command to plugins or loop execution.
///
/// Used by both `meta exec` (is_explicit_exec=true) and external subcommands
/// (is_explicit_exec=false).
fn handle_command_dispatch(
    command_args: Vec<String>,
    cli: &Cli,
    plugins: &SubprocessPluginManager,
    is_explicit_exec: bool,
) -> Result<()> {
    if command_args.is_empty() {
        if is_explicit_exec {
            eprintln!("Usage: meta exec <command> [args...]");
        } else {
            print_help_with_plugins(plugins, true);
        }
        std::process::exit(1);
    }

    // All meta flags come from clap globals (before the command).
    // Command args pass through untouched to avoid collisions with
    // identically-named flags (e.g., grep --include, git clone --depth).
    let include_filters: Vec<String> = cli.include.clone().unwrap_or_default();
    let exclude_filters: Vec<String> = cli.exclude.clone().unwrap_or_default();
    let recursive = cli.recursive;
    let dry_run = cli.dry_run;
    let depth = cli.depth;
    let parallel = cli.parallel;

    let command_str = command_args.join(" ");

    // Check if this is `git clone` - it doesn't require a .meta file because
    // its purpose is to clone the repo that contains the .meta file
    let is_git_clone = command_args
        .first()
        .map(|s| s == "git")
        .unwrap_or(false)
        && command_args
            .get(1)
            .map(|s| s == "clone")
            .unwrap_or(false);

    if is_git_clone {
        // Handle git clone directly via plugin without requiring .meta file
        let clone_args: Vec<String> = command_args.iter().skip(2).cloned().collect();

        let subprocess_options = PluginRequestOptions {
            json_output: cli.json,
            verbose: cli.verbose,
            parallel: false,
            dry_run,
            silent: cli.silent,
            recursive: false,
            depth: None,
            include_filters: None,
            exclude_filters: None,
        };

        if plugins.execute("git clone", &clone_args, &[], subprocess_options)? {
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

    // Context detection: if cwd is inside a worktree, auto-scope to its repos
    if !cli.primary {
        if let Some((task_name, task_dir, wt_paths)) =
            worktree::detect_worktree_context(&current_dir)
        {
            if cli.verbose {
                eprintln!(
                    "Detected worktree context: '{}' ({} repos)",
                    task_name,
                    wt_paths.len()
                );
            }

            // Try to find .meta config for full feature support
            if let Some((config_path, _format)) =
                find_meta_config(&task_dir, cli.config.as_ref())
            {
                let (meta_projects, ignore_list) = parse_meta_config(&config_path)?;

                // Build name→info lookup for tag filtering
                let project_map: std::collections::HashMap<&str, &ProjectInfo> = meta_projects
                    .iter()
                    .map(|p| (p.name.as_str(), p))
                    .collect();

                // Derive aliases from worktree paths and filter by tags
                let wt_directories: Vec<String> = wt_paths
                    .iter()
                    .filter(|path| {
                        if let Some(ref tag_filter) = cli.tag {
                            let alias = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| ".".to_string());
                            // Check if this repo's project has matching tags
                            if let Some(info) = project_map.get(alias.as_str()) {
                                let requested: Vec<&str> =
                                    tag_filter.split(',').map(|s| s.trim()).collect();
                                info.tags.iter().any(|t| requested.contains(&t.as_str()))
                            } else {
                                // Unknown projects pass through (no tags to filter on)
                                true
                            }
                        } else {
                            true
                        }
                    })
                    .map(|p| p.display().to_string())
                    .collect();

                if wt_directories.is_empty() {
                    eprintln!(
                        "{}: no projects match tag filter '{}' in worktree '{}'",
                        "warning".yellow().bold(),
                        cli.tag.as_deref().unwrap_or(""),
                        task_name
                    );
                    return Ok(());
                }

                // Check if the config is from the worktree itself or the primary checkout
                let config_in_worktree = config_path.starts_with(&task_dir);
                if cli.verbose && !config_in_worktree {
                    eprintln!(
                        "Using config from primary checkout: {}",
                        config_path.display()
                    );
                }

                let include_opt = if include_filters.is_empty() {
                    None
                } else {
                    Some(include_filters.clone())
                };
                let exclude_opt = if exclude_filters.is_empty() {
                    None
                } else {
                    Some(exclude_filters.clone())
                };

                let config = loop_lib::LoopConfig {
                    directories: wt_directories.clone(),
                    ignore: ignore_list,
                    include_filters: include_opt.clone(),
                    exclude_filters: exclude_opt.clone(),
                    verbose: cli.verbose,
                    silent: cli.silent,
                    parallel,
                    dry_run,
                    json_output: cli.json,
                    add_aliases_to_global_looprc: false,
                    spawn_stagger_ms: 0,
                };

                // Try plugin dispatch first (full feature parity with normal path)
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

                if plugins.execute(
                    &command_str,
                    &command_args,
                    &wt_directories,
                    subprocess_options,
                )? {
                    if cli.verbose {
                        println!(
                            "{}",
                            "Command handled by subprocess plugin (worktree).".green()
                        );
                    }
                } else if is_explicit_exec {
                    run(&config, &command_str)?;
                } else {
                    // Unrecognized command in worktree context
                    let first_cmd =
                        command_args.first().map(|s| s.as_str()).unwrap_or("");
                    eprintln!(
                        "{}: unrecognized command '{}'",
                        "error".red().bold(),
                        first_cmd
                    );
                    eprintln!();
                    eprintln!("To run '{}' across all repos:", command_str);
                    eprintln!("    meta exec {}", command_str);
                    eprintln!();
                    print_help_with_plugins(plugins, true);
                    std::process::exit(1);
                }
                return Ok(());
            }

            // No config found — degraded legacy path with warning
            if cli.verbose {
                eprintln!(
                    "{} No .meta config found for worktree '{}'. Tags, plugins, and dependency features unavailable.",
                    "warning:".yellow().bold(),
                    task_name
                );
            }

            let directories: Vec<String> =
                wt_paths.iter().map(|p| p.display().to_string()).collect();

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
                directories,
                ignore: vec![],
                include_filters: include_opt,
                exclude_filters: exclude_opt,
                verbose: cli.verbose,
                silent: cli.silent,
                parallel: false,
                dry_run,
                json_output: cli.json,
                add_aliases_to_global_looprc: false,
                spawn_stagger_ms: 0,
            };

            run(&config, &command_str)?;
            return Ok(());
        }
    }

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
    if recursive {
        if cli.verbose {
            let depth_str = depth.map_or("unlimited".to_string(), |d| d.to_string());
            println!("Recursive mode enabled, max depth: {}", depth_str);
        }
        let tree = config::walk_meta_tree(meta_dir, depth)?;
        project_paths = vec![".".to_string()];
        let flat = flatten_with_tag_filter(&tree, &cli.tag);
        project_paths.extend(
            flat.iter()
                .map(|p| meta_dir.join(p).to_string_lossy().to_string()),
        );
    }

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

    if plugins.execute(
        &command_str,
        &command_args,
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
    } else if is_explicit_exec {
        // User explicitly requested exec, run the command in all repos
        log::info!("Explicit exec requested, running command via loop");
        if cli.verbose {
            println!(
                "{}",
                "Running command via loop (explicit exec).".green()
            );
        }
        run(&config, &command_str)?;
    } else {
        // Unrecognized command - show actual help text so LLMs can self-correct
        let first_cmd = command_args.first().map(|s| s.as_str()).unwrap_or("");
        eprintln!(
            "{}: unrecognized command '{}'",
            "error".red().bold(),
            first_cmd
        );
        eprintln!();
        eprintln!("To run '{}' across all repos:", command_str);
        eprintln!("    meta exec {}", command_str);
        eprintln!();
        // Print the actual help text to stderr (not a reference to --help)
        print_help_with_plugins(plugins, true);
        std::process::exit(1);
    }

    Ok(())
}

// === Plugin Management ===

/// Handle plugin management subcommands with typed args.
fn handle_plugin_command(command: Option<PluginCommands>, verbose: bool, json: bool) -> Result<()> {
    use registry::{PluginInstaller, RegistryClient};

    let command = match command {
        Some(cmd) => cmd,
        None => {
            println!("Usage: meta plugin <command>");
            println!();
            println!("Commands:");
            println!("  search <query>   Search for plugins in the registry");
            println!("  install <name>   Install a plugin from the registry");
            println!("  list             List installed plugins");
            println!("  uninstall <name> Uninstall a plugin");
            return Ok(());
        }
    };

    match command {
        PluginCommands::Search { query } => {
            let client = RegistryClient::new(verbose)?;
            let results = client.search(&query)?;

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
        PluginCommands::Install { name } => {
            let client = RegistryClient::new(verbose)?;
            let metadata = client.fetch_plugin_metadata(&name)?;

            let installer = PluginInstaller::new(verbose)?;
            installer.install(&metadata)?;

            if !json {
                println!(
                    "Successfully installed {} v{}",
                    metadata.name, metadata.version
                );
            }
        }
        PluginCommands::List => {
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
        PluginCommands::Uninstall { name } => {
            let installer = PluginInstaller::new(verbose)?;
            installer.uninstall(&name)?;

            if !json {
                println!("Successfully uninstalled {name}");
            }
        }
    }

    Ok(())
}

// === Tree Utilities ===

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

// === Tests ===

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
