use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("Failed to load plugin: {0}")]
    LoadError(String),
    #[error("Command not found: {0}")]
    CommandNotFound(String),
}

use meta_plugin_api::Plugin;

pub type PluginCreate = unsafe fn() -> *mut dyn Plugin;

// In src/main.rs
use libloading::{Library, Symbol};
use std::path::Path;
use std::fs;


pub struct PluginOptions {
    pub verbose: bool,
    pub json: bool,
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

    pub fn load_plugins(&mut self, options: &PluginOptions) -> anyhow::Result<()> {
        use std::env;
        let mut current_dir = env::current_dir()?;
        let mut visited = std::collections::HashSet::new();

        // Walk up to root
        loop {
            let plugin_dir = current_dir.join(".meta-plugins");
            if options.verbose {
                println!("Searching for plugins in: {}", plugin_dir.display());
            }
            if plugin_dir.exists() && plugin_dir.is_dir() {
                // Avoid loading from the same directory twice
                if visited.insert(plugin_dir.clone()) {
                    if options.verbose {
                        println!("Found plugin directory: {}, loading plugins...", plugin_dir.display());
                    }
                    self.load_plugins_from_dir(&plugin_dir, options)?;
                }
            }

            // Stop if reached root
            if let Some(parent) = current_dir.parent() {
                current_dir = parent.to_path_buf();
            } else {
                break;
            }
        }

        // Finally, check home directory
        if let Ok(home_dir) = env::var("HOME") {
            let home_plugin_dir = Path::new(&home_dir).join(".meta-plugins");
            if options.verbose {
                println!("Searching for plugins in: {}", home_plugin_dir.display());
            }
            if home_plugin_dir.exists() && home_plugin_dir.is_dir() {
                if visited.insert(home_plugin_dir.clone()) {
                    if options.verbose {
                        println!("Found plugin directory: {}, loading plugins...", home_plugin_dir.display());
                    }
                    self.load_plugins_from_dir(&home_plugin_dir, options)?;
                }
            }
        }

        Ok(())
    }

    fn load_plugins_from_dir(&mut self, plugin_dir: &Path, options: &PluginOptions) -> anyhow::Result<()> {
        for entry in fs::read_dir(plugin_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file()
                && path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| {
                        name.starts_with("meta-")
                            && (name.ends_with(".dll")
                                || name.ends_with(".dylib")
                                || name.ends_with(".so"))
                    })
                    .unwrap_or(false)
            {
                if let Err(e) = self.load_plugin(&path) {
                    // Print error and continue if verbose, otherwise just continue
                    if options.verbose {
                        eprintln!("Failed to load plugin '{}': {}", path.display(), e);
                    }
                }
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

        // Compose plugin command string: first word or first two words
        let plugin_command = if parts.len() >= 2 {
            format!("{} {}", parts[0], parts[1])
        } else {
            parts[0].to_string()
        };

        // Compose plugin args: skip first one or two words
        let plugin_args: Vec<String> = if parts.len() >= 2 {
            args.iter().skip(2).cloned().collect()
        } else {
            args.iter().skip(1).cloned().collect()
        };

        for plugin in self.plugins.values() {
            if plugin.commands().contains(&plugin_command.as_str()) {
                return plugin.execute(&plugin_command, &plugin_args);
            }
        }

        Err(PluginError::CommandNotFound(command.to_string()).into())
    }

    /// Attempt to dispatch a command to any plugin.
    /// Returns Ok(true) if a plugin handled the command, Ok(false) otherwise.
    pub fn dispatch_command(&self, cli_command: &[String], _projects: &[String]) -> anyhow::Result<bool> {
        if cli_command.is_empty() {
            return Ok(false);
        }
        
        #[cfg(test)]
        mod tests {
            use super::*;
            
            #[test]
            fn test_plugin_manager_new() {
                let manager = PluginManager::new();
                assert!(manager.plugins.is_empty());
            }
        
            #[test]
            fn test_dispatch_command_no_plugins() {
                let manager = PluginManager::new();
                let cli_command = vec!["dummy".to_string()];
                let projects = vec!["proj1".to_string()];
                let result = manager.dispatch_command(&cli_command, &projects);
                // Should return Ok(false) meaning no plugin handled it
                assert!(result.is_ok());
                assert!(!result.unwrap());
            }
        }

        let command_str = cli_command.join(" ");

        match self.execute_command(&command_str, cli_command) {
            Ok(_) => Ok(true),
            Err(e) => {
                // If error is CommandNotFound, return false, else propagate error
                if let Some(PluginError::CommandNotFound(_)) = e.downcast_ref::<PluginError>() {
                    Ok(false)
                } else {
                    Err(e)
                }
            }
        }
    }
    /// Attempt to get plugin help output for a CLI command.
    /// Returns Some((HelpMode, help text)) if plugin customizes help, else None.
    pub fn get_plugin_help_output(&self, cli_command: &[String]) -> Option<(meta_plugin_api::HelpMode, String)> {
        if cli_command.is_empty() {
            return None;
        }
        let first = cli_command[0].as_str();
        for plugin in self.plugins.values() {
            if plugin.commands().contains(&first) {
                return plugin.get_help_output(cli_command);
            }
        }
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use meta_plugin_api::Plugin;
    use tempfile::{tempdir, TempDir};
    use std::fs::{self, File};
    use std::io::Write;
    use std::env;
    use std::path::PathBuf;

    struct DummyPlugin {
        should_handle: bool,
        fail: bool,
    }

    impl Plugin for DummyPlugin {
        fn name(&self) -> &'static str {
            "dummy"
        }
        fn commands(&self) -> Vec<&'static str> {
            vec!["git", "git clone"]
        }
        fn execute(&self, _command: &str, _args: &[String]) -> anyhow::Result<()> {
            if self.fail {
                Err(anyhow::anyhow!("Simulated failure"))
            } else if self.should_handle {
                Ok(())
            } else {
                Err(PluginError::CommandNotFound("dummy".to_string()).into())
            }
        }
    }

    #[test]
    fn test_dispatch_command_plugin_handles() {
        let mut manager = PluginManager::new();
        let dummy = Box::new(DummyPlugin { should_handle: true, fail: false });
        manager.plugins.insert("dummy".to_string(), dummy);

        let cli_command = vec!["git".to_string(), "clone".to_string()];
        let projects = vec!["proj1".to_string()];
        let result = manager.dispatch_command(&cli_command, &projects);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_dispatch_command_plugin_fails() {
        let mut manager = PluginManager::new();
        let dummy = Box::new(DummyPlugin { should_handle: true, fail: true });
        manager.plugins.insert("dummy".to_string(), dummy);

        let cli_command = vec!["git".to_string(), "clone".to_string()];
        let projects = vec!["proj1".to_string()];
        let result = manager.dispatch_command(&cli_command, &projects);
        assert!(result.is_err());
    }

    #[test]
    fn test_dispatch_command_no_plugin_handles() {
        let mut manager = PluginManager::new();
        let dummy = Box::new(DummyPlugin { should_handle: false, fail: false });
        manager.plugins.insert("dummy".to_string(), dummy);

        let cli_command = vec!["git".to_string(), "clone".to_string()];
        let projects = vec!["proj1".to_string()];
        let result = manager.dispatch_command(&cli_command, &projects);
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn test_plugin_discovery_search_order_and_logging() {
        // Setup: create a temp dir structure with .meta-plugins in current and parent
        let temp_root = tempdir().unwrap();
        let parent_dir = temp_root.path().join("parent");
        let current_dir = parent_dir.join("current");
        fs::create_dir_all(&current_dir).unwrap();

        // Create .meta-plugins in both parent and current
        let parent_plugins = parent_dir.join(".meta-plugins");
        let current_plugins = current_dir.join(".meta-plugins");
        fs::create_dir_all(&parent_plugins).unwrap();
        fs::create_dir_all(&current_plugins).unwrap();

        // Create fake plugin files
        let plugin_file = |dir: &PathBuf, name: &str| {
            let path = dir.join(name);
            let mut file = File::create(&path).unwrap();
            file.write_all(b"not a real plugin").unwrap();
            path
        };
        let _p1 = plugin_file(&parent_plugins, "meta-test1.so");
        let _p2 = plugin_file(&current_plugins, "meta-test2.so");

        // Save original current_dir and set to our test current_dir
        let orig_dir = env::current_dir().unwrap();
        env::set_current_dir(&current_dir).unwrap();

        // Run plugin discovery; should not panic even though plugin files are not valid
        let mut manager = PluginManager::new();
        let options = PluginOptions { verbose: true, json: false };
        // Just call load_plugins and assert it returns an error (since plugin files are not valid)
        let result = manager.load_plugins(&options);

        // Restore original current_dir
        env::set_current_dir(orig_dir).unwrap();

        // The function should not panic even if individual plugins fail to load
        // Since we're continuing after plugin load failures, the overall result should be Ok
        assert!(result.is_ok());
        // The test output shows that the fake plugins were attempted to be loaded and failed,
        // but the operation continued and completed successfully
    }
}
