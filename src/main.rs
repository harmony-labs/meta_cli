use anyhow::Result;
use clap::Parser;
use loop_lib::run;
use std::path::PathBuf;
use serde_json::Value;
use std::fs;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    
    let meta_file_path = cli.config.unwrap_or_else(|| PathBuf::from(".meta"));
    let command = cli.command.join(" ");
    
    // Parse the .meta file
    let config_str = fs::read_to_string(&meta_file_path)?;
    let meta_config: Value = serde_json::from_str(&config_str)?;
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
        verbose: false,
        silent: false,
        parallel: false,
    };

    run(&config, &command)?;

    Ok(())
}
