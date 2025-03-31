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
    #[arg(long, help = "Add shell aliases to the global .looprc file, enabling aliases within commands")]
    add_aliases_to_global_looprc: bool,

    #[arg(trailing_var_arg = true)]
    command: Vec<String>,

    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    #[arg(short, long, help = "Specify directories to exclude (adds to config file exclusions)")]
    exclude: Option<Vec<String>>,

    #[arg(short, long, help = "Specify directories to include (overrides config file)")]
    include: Option<Vec<String>>,

    #[arg(short, long, help = "Enable silent mode (suppress all output)")]
    silent: bool,

    #[arg(short, long, action, help = "Enable verbose output")]
    verbose: bool,
}

fn main() -> Result<()> {
    let mut plugin_manager = PluginManager::new();
    plugin_manager.load_plugins()?;
    
    let cli = Cli::parse();
    
    let meta_file_path = PathBuf::from(cli.config.unwrap_or_else(|| PathBuf::from(".meta")));

    if cli.command.is_empty() {
        Cli::command().print_help()?;
        std::process::exit(0);
    }

    let command = cli.command.join(" ");
    
    // Parse the .meta file
    let absolute_path = std::env::current_dir()?.join(&meta_file_path);

    if cli.verbose {
        println!("{}", "Verbose mode enabled".green());
        println!("\nResolved config file path: {}", absolute_path.display());
        println!("Executing command: {}", command);
    }

    let config_str = fs::read_to_string(&absolute_path)
        .with_context(|| format!("Failed to read meta config file: '{}'", absolute_path.display()))?;
    let meta_config: Value = serde_json::from_str(&config_str)
        .with_context(|| format!("Failed to parse meta config file: {}", absolute_path.display()))?;
    let meta_projects = meta_config["projects"].as_object()
        .unwrap_or(&serde_json::Map::new())
        .keys()
        .cloned()
        .collect::<Vec<String>>();

    let mut projects = vec![".".to_string()];
    projects.extend(meta_projects);

    let config = loop_lib::LoopConfig {
        add_aliases_to_global_looprc: false,
        directories: projects,
        ignore: meta_config["ignore"].as_array()
            .unwrap_or(&vec![])
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect::<Vec<String>>(),
        verbose: cli.verbose,
        silent: false,
    };

    run(&config, &command)?;

    Ok(())
}
