use anyhow::{Context, Result};
use clap::{Parser, CommandFactory};
use loop_lib::run;
use std::path::PathBuf;
use serde_json::Value;
use std::fs;
use colored::*;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    #[arg(short, long, action, help = "Enable verbose output")]
    verbose: bool,

    #[arg(short, long, help = "Specify directories to include (overrides config file)")]
    include: Option<Vec<String>>,

    #[arg(short, long, help = "Specify directories to exclude (adds to config file exclusions)")]
    exclude: Option<Vec<String>>,

    #[arg(short, long, help = "Enable silent mode (suppress all output)")]
    silent: bool,

    #[arg(long, help = "Execute commands in parallel")]
    parallel: bool,

    #[arg(long, help = "Add shell aliases to the global .looprc file")]
    add_aliases_to_global_looprc: bool,
    command: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    
    let meta_file_path = PathBuf::from(cli.config.unwrap_or_else(|| PathBuf::from(".meta")));
    if cli.command.is_empty() {
        Cli::command().print_help()?;
        std::process::exit(0);
    }
    let command = cli.command.join(" ");
    
    if cli.verbose {
        println!("{}", "Verbose mode enabled".green());
        println!("Using config file: {}", meta_file_path.display());
        println!("Executing command: {}", command);
    }
    
    // Parse the .meta file
    let config_str = fs::read_to_string(&meta_file_path)
        .with_context(|| format!("Failed to read config file: {:?}", meta_file_path))?;
    let meta_config: Value = serde_json::from_str(&config_str)
        .with_context(|| format!("Failed to parse config file: {:?}", meta_file_path))?;
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
        parallel: false,
    };

    run(&config, &command)?;

    Ok(())
}
