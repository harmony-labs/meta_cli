use std::any::Any;
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("Failed to load plugin: {0}")]
    LoadError(String),
    #[error("Command not found: {0}")]
    CommandNotFound(String),
}

pub trait Plugin: Any {
    fn name(&self) -> &'static str;
    fn commands(&self) -> Vec<&'static str>;
    fn execute(&self, command: &str, args: &[String]) -> anyhow::Result<()>;
}

pub type PluginCreate = unsafe fn() -> *mut dyn Plugin;

// In src/main.rs
use libloading::{Library, Symbol};
use std::path::{Path, PathBuf};
use std::fs;
use clap::Parser;

#[derive(Parser)]
#[command(name = "meta")]
struct Cli {
    command: Option<String>,
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
}

pub struct PluginManager {
    plugins: HashMap<String, Box<dyn Plugin>>,
    _libraries: Vec<Library>, // Keep libraries loaded
}

impl PluginManager {
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
            _libraries: Vec::new(),
        }
    }

    pub fn load_plugins(&mut self) -> anyhow::Result<()> {
        let plugin_dir = Path::new(".meta-plugins");
        if !plugin_dir.exists() {
            return Ok(());
        }

        for entry in fs::read_dir(plugin_dir)? {
            let entry = entry?;
            let path = entry.path();
            
            if path.is_file() && path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("meta-") && name.ends_with(".dll"))
                .unwrap_or(false) 
            {
                self.load_plugin(&path)?;
            }
        }
        Ok(())
    }

    pub fn load_plugin(&mut self, path: &Path) -> anyhow::Result<()> {
        unsafe {
            let library = Library::new(path)?;
            let creator: Symbol<PluginCreate> = library.get(b"_plugin_create")?;
            let plugin = Box::from_raw(creator());
            
            let name = plugin.name().to_string();
            self.plugins.insert(name, plugin);
            self._libraries.push(library);
        }
        Ok(())
    }

    pub fn execute_command(&self, command: &str, args: &[String]) -> anyhow::Result<()> {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            return Ok(());
        }

        for plugin in self.plugins.values() {
            if plugin.commands().contains(&parts[0]) {
                return plugin.execute(command, args);
            }
        }

        Err(PluginError::CommandNotFound(command.to_string()).into())
    }
}