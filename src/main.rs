use anyhow::Result;
use clap::Parser;
use loop_lib::{parse_config, run, LoopConfig};
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    #[arg(short, long)]
    include: Option<Vec<String>>,

    #[arg(short, long)]
    exclude: Option<Vec<String>>,

    #[arg(short, long)]
    verbose: bool,

    #[arg(short, long)]
    silent: bool,

    #[arg(long)]
    parallel: bool,

    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    
    let config_path = cli.config.unwrap_or_else(|| PathBuf::from(".looprc"));
    let mut config = parse_config(&config_path)?;

    // Update config with CLI options
    if let Some(include) = cli.include {
        config.directories = include;
    }
    if let Some(exclude) = cli.exclude {
        config.ignore.extend(exclude);
    }
    config.verbose = cli.verbose;
    config.silent = cli.silent;
    config.parallel = cli.parallel;

    // If no directories specified, use current and all child directories
    if config.directories.is_empty() {
        config.directories = vec![".".to_string()];
    }

    let command = cli.command.join(" ");
    run(&config, &command)?;

    Ok(())
}
