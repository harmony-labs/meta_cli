//! Subprocess-based plugin system for meta.
//!
//! Plugins are standalone executables that communicate via JSON over stdin/stdout.
//! This approach provides better isolation, language flexibility, and simpler debugging.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Information about a subprocess plugin
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub commands: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Request sent to a plugin for command execution
#[derive(Debug, Serialize)]
pub struct PluginRequest {
    pub command: String,
    pub args: Vec<String>,
    pub projects: Vec<String>,
    pub cwd: String,
    #[serde(default)]
    pub options: PluginRequestOptions,
}

#[derive(Debug, Default, Serialize)]
pub struct PluginRequestOptions {
    pub json_output: bool,
    pub verbose: bool,
    pub parallel: bool,
}

/// Response from a plugin after command execution
#[derive(Debug, Deserialize)]
pub struct PluginResponse {
    pub success: bool,
    #[serde(default)]
    pub exit_code: i32,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// A discovered subprocess plugin
#[derive(Debug, Clone)]
pub struct SubprocessPlugin {
    pub path: PathBuf,
    pub info: PluginInfo,
}

/// Manager for subprocess-based plugins
pub struct SubprocessPluginManager {
    plugins: HashMap<String, SubprocessPlugin>,
    verbose: bool,
}

impl SubprocessPluginManager {
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
            verbose: false,
        }
    }

    /// Discover and load all subprocess plugins
    pub fn discover_plugins(&mut self, verbose: bool) -> Result<()> {
        self.verbose = verbose;
        let mut visited = std::collections::HashSet::new();

        // Search in .meta-plugins directories walking up from cwd
        let mut current_dir = std::env::current_dir()?;
        loop {
            let plugin_dir = current_dir.join(".meta-plugins");
            if plugin_dir.exists() && plugin_dir.is_dir() && visited.insert(plugin_dir.clone()) {
                self.scan_directory(&plugin_dir)?;
            }
            if let Some(parent) = current_dir.parent() {
                current_dir = parent.to_path_buf();
            } else {
                break;
            }
        }

        // Search in ~/.meta-plugins
        if let Ok(home) = std::env::var("HOME") {
            let home_plugins = Path::new(&home).join(".meta-plugins");
            if home_plugins.exists() && visited.insert(home_plugins.clone()) {
                self.scan_directory(&home_plugins)?;
            }
        }

        // Search in PATH for meta-* executables
        if let Ok(path_var) = std::env::var("PATH") {
            for path_dir in path_var.split(':') {
                let dir = Path::new(path_dir);
                if dir.exists() && visited.insert(dir.to_path_buf()) {
                    self.scan_path_directory(dir)?;
                }
            }
        }

        Ok(())
    }

    /// Scan a .meta-plugins directory for plugin executables
    fn scan_directory(&mut self, dir: &Path) -> Result<()> {
        if self.verbose {
            println!("Scanning for subprocess plugins in: {}", dir.display());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            // Look for executables named meta-*
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("meta-") && !name.ends_with(".dylib") && !name.ends_with(".so") && !name.ends_with(".dll") {
                    self.try_load_plugin(&path)?;
                }
            }
        }
        Ok(())
    }

    /// Scan a PATH directory for meta-* executables
    fn scan_path_directory(&mut self, dir: &Path) -> Result<()> {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("meta-") && is_executable(&path) {
                        self.try_load_plugin(&path)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Try to load a plugin by querying its info
    fn try_load_plugin(&mut self, path: &Path) -> Result<()> {
        if !is_executable(path) {
            return Ok(());
        }

        // Query plugin info
        let output = Command::new(path)
            .arg("--meta-plugin-info")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let info: PluginInfo = serde_json::from_slice(&output.stdout)
                    .with_context(|| format!("Failed to parse plugin info from {}", path.display()))?;

                if self.verbose {
                    println!("  Found plugin: {} v{} ({})", info.name, info.version, path.display());
                }

                // Don't override if already loaded (first one wins)
                if !self.plugins.contains_key(&info.name) {
                    self.plugins.insert(info.name.clone(), SubprocessPlugin {
                        path: path.to_path_buf(),
                        info,
                    });
                }
            }
            _ => {
                // Not a valid plugin, ignore silently
            }
        }
        Ok(())
    }

    /// Check if any plugin handles the given command
    pub fn handles_command(&self, command: &str) -> bool {
        let cmd_parts: Vec<&str> = command.split_whitespace().collect();
        if cmd_parts.is_empty() {
            return false;
        }

        // Try matching "git status" style (two words)
        let two_word = if cmd_parts.len() >= 2 {
            format!("{} {}", cmd_parts[0], cmd_parts[1])
        } else {
            String::new()
        };

        for plugin in self.plugins.values() {
            for plugin_cmd in &plugin.info.commands {
                if plugin_cmd == command || plugin_cmd == &two_word || plugin_cmd == cmd_parts[0] {
                    return true;
                }
            }
        }
        false
    }

    /// Execute a command via the appropriate plugin
    pub fn execute(
        &self,
        command: &str,
        args: &[String],
        projects: &[String],
        options: PluginRequestOptions,
    ) -> Result<bool> {
        let cmd_parts: Vec<&str> = command.split_whitespace().collect();
        if cmd_parts.is_empty() {
            return Ok(false);
        }

        // Find the right plugin
        let two_word = if cmd_parts.len() >= 2 {
            format!("{} {}", cmd_parts[0], cmd_parts[1])
        } else {
            String::new()
        };

        for plugin in self.plugins.values() {
            for plugin_cmd in &plugin.info.commands {
                if plugin_cmd == command || plugin_cmd == &two_word || plugin_cmd == cmd_parts[0] {
                    return self.execute_plugin(plugin, command, args, projects, &options);
                }
            }
        }
        Ok(false)
    }

    /// Execute a specific plugin
    fn execute_plugin(
        &self,
        plugin: &SubprocessPlugin,
        command: &str,
        args: &[String],
        projects: &[String],
        options: &PluginRequestOptions,
    ) -> Result<bool> {
        let request = PluginRequest {
            command: command.to_string(),
            args: args.to_vec(),
            projects: projects.to_vec(),
            cwd: std::env::current_dir()?.to_string_lossy().to_string(),
            options: PluginRequestOptions {
                json_output: options.json_output,
                verbose: options.verbose,
                parallel: options.parallel,
            },
        };

        let request_json = serde_json::to_string(&request)?;

        if self.verbose {
            println!("Executing plugin {} for command '{}'", plugin.info.name, command);
        }

        let mut child = Command::new(&plugin.path)
            .arg("--meta-plugin-exec")
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())  // Let plugin output directly
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("Failed to execute plugin {}", plugin.path.display()))?;

        // Send request to plugin's stdin
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(request_json.as_bytes())?;
        }

        let status = child.wait()?;

        if !status.success() {
            anyhow::bail!("Plugin {} exited with status {}", plugin.info.name, status);
        }

        Ok(true)
    }

    /// Get list of all available commands from all plugins
    pub fn available_commands(&self) -> Vec<(&str, &str)> {
        let mut commands = Vec::new();
        for plugin in self.plugins.values() {
            for cmd in &plugin.info.commands {
                commands.push((cmd.as_str(), plugin.info.name.as_str()));
            }
        }
        commands
    }
}

/// Check if a file is executable
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = path.metadata() {
        let mode = metadata.permissions().mode();
        mode & 0o111 != 0 && metadata.is_file()
    } else {
        false
    }
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_manager_new() {
        let manager = SubprocessPluginManager::new();
        assert!(manager.plugins.is_empty());
    }

    #[test]
    fn test_handles_command_empty() {
        let manager = SubprocessPluginManager::new();
        assert!(!manager.handles_command(""));
        assert!(!manager.handles_command("git status"));
    }

    #[test]
    fn test_plugin_info_serialization() {
        let info = PluginInfo {
            name: "test".to_string(),
            version: "1.0.0".to_string(),
            commands: vec!["test cmd".to_string()],
            description: Some("A test plugin".to_string()),
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"name\":\"test\""));
        assert!(json.contains("\"version\":\"1.0.0\""));

        // Deserialize back
        let parsed: PluginInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "test");
        assert_eq!(parsed.version, "1.0.0");
        assert_eq!(parsed.commands, vec!["test cmd"]);
    }

    #[test]
    fn test_plugin_request_serialization() {
        let request = PluginRequest {
            command: "git status".to_string(),
            args: vec!["--verbose".to_string()],
            projects: vec!["project1".to_string(), "project2".to_string()],
            cwd: "/home/user/workspace".to_string(),
            options: PluginRequestOptions {
                json_output: true,
                verbose: false,
                parallel: true,
            },
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"command\":\"git status\""));
        assert!(json.contains("\"json_output\":true"));
        assert!(json.contains("\"parallel\":true"));
    }

    #[test]
    fn test_plugin_request_options_default() {
        let options = PluginRequestOptions::default();
        assert!(!options.json_output);
        assert!(!options.verbose);
        assert!(!options.parallel);
    }

    #[test]
    fn test_handles_command_matching() {
        let mut manager = SubprocessPluginManager::new();

        // Manually add a plugin for testing
        let plugin = SubprocessPlugin {
            path: std::path::PathBuf::from("/fake/path/meta-test"),
            info: PluginInfo {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                commands: vec!["test".to_string(), "test run".to_string()],
                description: None,
            },
        };
        manager.plugins.insert("test".to_string(), plugin);

        // Should match single-word command
        assert!(manager.handles_command("test"));
        // Should match two-word command
        assert!(manager.handles_command("test run"));
        // Should not match unknown command
        assert!(!manager.handles_command("unknown"));
    }

    #[test]
    fn test_available_commands() {
        let mut manager = SubprocessPluginManager::new();

        let plugin = SubprocessPlugin {
            path: std::path::PathBuf::from("/fake/path/meta-git"),
            info: PluginInfo {
                name: "git".to_string(),
                version: "1.0.0".to_string(),
                commands: vec!["git status".to_string(), "git pull".to_string()],
                description: None,
            },
        };
        manager.plugins.insert("git".to_string(), plugin);

        let commands = manager.available_commands();
        assert_eq!(commands.len(), 2);
        assert!(commands.iter().any(|(cmd, _)| *cmd == "git status"));
        assert!(commands.iter().any(|(cmd, _)| *cmd == "git pull"));
    }

    #[test]
    fn test_is_executable_nonexistent() {
        let path = std::path::Path::new("/nonexistent/path/to/binary");
        assert!(!is_executable(path));
    }
}
