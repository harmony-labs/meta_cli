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
    /// Help information for the plugin (optional - for backward compatibility)
    #[serde(default)]
    pub help: Option<PluginHelp>,
}

/// Help information for a plugin
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHelp {
    /// Usage string (e.g., "meta git <command> [args...]")
    pub usage: String,
    /// Command descriptions (command name -> description)
    #[serde(default)]
    pub commands: std::collections::HashMap<String, String>,
    /// Example usage strings
    #[serde(default)]
    pub examples: Vec<String>,
    /// Additional note (e.g., how to run raw commands)
    #[serde(default)]
    pub note: Option<String>,
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
#[allow(dead_code)]
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

impl Default for SubprocessPluginManager {
    fn default() -> Self {
        Self::new()
    }
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
                if name.starts_with("meta-")
                    && !name.ends_with(".dylib")
                    && !name.ends_with(".so")
                    && !name.ends_with(".dll")
                {
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
                let info: PluginInfo =
                    serde_json::from_slice(&output.stdout).with_context(|| {
                        format!("Failed to parse plugin info from {}", path.display())
                    })?;

                if self.verbose {
                    println!(
                        "  Found plugin: {} v{} ({})",
                        info.name,
                        info.version,
                        path.display()
                    );
                }

                // Don't override if already loaded (first one wins)
                if !self.plugins.contains_key(&info.name) {
                    self.plugins.insert(
                        info.name.clone(),
                        SubprocessPlugin {
                            path: path.to_path_buf(),
                            info,
                        },
                    );
                }
            }
            _ => {
                // Not a valid plugin, ignore silently
            }
        }
        Ok(())
    }

    /// Check if any plugin handles the given command
    #[allow(dead_code)]
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
            println!(
                "Executing plugin {} for command '{}'",
                plugin.info.name, command
            );
        }

        let mut child = Command::new(&plugin.path)
            .arg("--meta-plugin-exec")
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit()) // Let plugin output directly
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
    #[allow(dead_code)]
    pub fn available_commands(&self) -> Vec<(&str, &str)> {
        let mut commands = Vec::new();
        for plugin in self.plugins.values() {
            for cmd in &plugin.info.commands {
                commands.push((cmd.as_str(), plugin.info.name.as_str()));
            }
        }
        commands
    }

    /// Get a plugin by name
    #[allow(dead_code)]
    pub fn get_plugin(&self, name: &str) -> Option<&SubprocessPlugin> {
        self.plugins.get(name)
    }

    /// Get a plugin that handles a specific command (e.g., "git" for "git status")
    #[allow(dead_code)]
    pub fn get_plugin_for_command(&self, command: &str) -> Option<&SubprocessPlugin> {
        let cmd_parts: Vec<&str> = command.split_whitespace().collect();
        if cmd_parts.is_empty() {
            return None;
        }

        // First word is likely the plugin name
        let plugin_name = cmd_parts[0];
        self.plugins.get(plugin_name)
    }

    /// Get list of all plugins with their descriptions
    pub fn list_plugins(&self) -> Vec<(&str, &str, &str)> {
        let mut plugins: Vec<_> = self
            .plugins
            .values()
            .map(|p| {
                (
                    p.info.name.as_str(),
                    p.info.version.as_str(),
                    p.info
                        .description
                        .as_deref()
                        .unwrap_or("No description available"),
                )
            })
            .collect();
        plugins.sort_by(|a, b| a.0.cmp(b.0));
        plugins
    }

    /// Get help text for a specific plugin
    pub fn get_plugin_help(&self, plugin_name: &str) -> Option<String> {
        let plugin = self.plugins.get(plugin_name)?;

        // Try to get help by executing plugin with --help
        let output = Command::new(&plugin.path)
            .arg("--help")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();

        if let Ok(output) = output {
            if output.status.success() {
                return Some(String::from_utf8_lossy(&output.stdout).to_string());
            }
        }

        // Fall back to generating help from plugin info
        Some(self.generate_fallback_help(plugin))
    }

    /// Generate help text from PluginInfo when plugin doesn't support --help
    fn generate_fallback_help(&self, plugin: &SubprocessPlugin) -> String {
        let info = &plugin.info;
        let mut help = String::new();
        let name = &info.name;
        let version = &info.version;
        let description = info.description.as_deref().unwrap_or("Meta CLI plugin");

        // Header
        help.push_str(&format!("meta-{name} v{version} - {description}\n\n"));

        // If plugin has structured help, use it
        if let Some(ref plugin_help) = info.help {
            help.push_str("USAGE:\n");
            let usage = &plugin_help.usage;
            help.push_str(&format!("    {usage}\n\n"));

            if !plugin_help.commands.is_empty() {
                help.push_str("COMMANDS:\n");
                let mut cmds: Vec<_> = plugin_help.commands.iter().collect();
                cmds.sort_by(|a, b| a.0.cmp(b.0));
                for (cmd, desc) in cmds {
                    help.push_str(&format!("    {cmd:<16} {desc}\n"));
                }
                help.push('\n');
            }

            if !plugin_help.examples.is_empty() {
                help.push_str("EXAMPLES:\n");
                for example in &plugin_help.examples {
                    help.push_str(&format!("    {example}\n"));
                }
                help.push('\n');
            }

            if let Some(ref note) = plugin_help.note {
                help.push_str(&format!("NOTE:\n    {note}\n"));
            }
        } else {
            // Basic fallback from command list
            help.push_str("USAGE:\n");
            help.push_str(&format!("    meta {name} <command> [args...]\n\n"));

            if !info.commands.is_empty() {
                help.push_str("COMMANDS:\n");
                let prefix = format!("{name} ");
                for cmd in &info.commands {
                    // Strip "git " prefix for display
                    let display_cmd = cmd.strip_prefix(&prefix).unwrap_or(cmd);
                    help.push_str(&format!("    {display_cmd}\n"));
                }
                help.push('\n');
            }

            help.push_str(&format!(
                "NOTE:\n    To run raw {name} commands: meta exec -- {name} <command>\n"
            ));
        }

        help
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
            help: None,
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
                help: None,
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
                help: None,
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

    #[test]
    fn test_generate_fallback_help_without_structured_help() {
        let manager = SubprocessPluginManager::new();
        let plugin = SubprocessPlugin {
            path: std::path::PathBuf::from("/fake/path/meta-test"),
            info: PluginInfo {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                commands: vec!["test run".to_string(), "test check".to_string()],
                description: Some("A test plugin".to_string()),
                help: None,
            },
        };

        let help = manager.generate_fallback_help(&plugin);
        assert!(help.contains("meta-test v1.0.0"));
        assert!(help.contains("A test plugin"));
        assert!(help.contains("USAGE:"));
        assert!(help.contains("meta test <command>"));
        assert!(help.contains("COMMANDS:"));
        assert!(help.contains("run"));
        assert!(help.contains("check"));
        assert!(help.contains("NOTE:"));
        assert!(help.contains("meta exec -- test"));
    }

    #[test]
    fn test_generate_fallback_help_with_structured_help() {
        let manager = SubprocessPluginManager::new();
        let mut commands = std::collections::HashMap::new();
        commands.insert("build".to_string(), "Build the project".to_string());
        commands.insert("test".to_string(), "Run tests".to_string());

        let plugin = SubprocessPlugin {
            path: std::path::PathBuf::from("/fake/path/meta-rust"),
            info: PluginInfo {
                name: "rust".to_string(),
                version: "2.0.0".to_string(),
                commands: vec!["rust build".to_string()],
                description: Some("Rust plugin".to_string()),
                help: Some(PluginHelp {
                    usage: "meta rust <command> [args...]".to_string(),
                    commands,
                    examples: vec![
                        "meta rust build".to_string(),
                        "meta rust test --all".to_string(),
                    ],
                    note: Some("Custom note here".to_string()),
                }),
            },
        };

        let help = manager.generate_fallback_help(&plugin);
        assert!(help.contains("meta-rust v2.0.0"));
        assert!(help.contains("Rust plugin"));
        assert!(help.contains("USAGE:"));
        assert!(help.contains("meta rust <command> [args...]"));
        assert!(help.contains("COMMANDS:"));
        assert!(help.contains("build"));
        assert!(help.contains("Build the project"));
        assert!(help.contains("test"));
        assert!(help.contains("Run tests"));
        assert!(help.contains("EXAMPLES:"));
        assert!(help.contains("meta rust build"));
        assert!(help.contains("meta rust test --all"));
        assert!(help.contains("NOTE:"));
        assert!(help.contains("Custom note here"));
    }

    #[test]
    fn test_generate_fallback_help_no_description() {
        let manager = SubprocessPluginManager::new();
        let plugin = SubprocessPlugin {
            path: std::path::PathBuf::from("/fake/path/meta-simple"),
            info: PluginInfo {
                name: "simple".to_string(),
                version: "0.1.0".to_string(),
                commands: vec![],
                description: None,
                help: None,
            },
        };

        let help = manager.generate_fallback_help(&plugin);
        assert!(help.contains("meta-simple v0.1.0"));
        assert!(help.contains("Meta CLI plugin")); // Default description
    }

    #[test]
    fn test_list_plugins_sorted() {
        let mut manager = SubprocessPluginManager::new();

        // Add plugins in non-alphabetical order
        manager.plugins.insert(
            "zebra".to_string(),
            SubprocessPlugin {
                path: std::path::PathBuf::from("/fake/meta-zebra"),
                info: PluginInfo {
                    name: "zebra".to_string(),
                    version: "1.0.0".to_string(),
                    commands: vec![],
                    description: Some("Z plugin".to_string()),
                    help: None,
                },
            },
        );
        manager.plugins.insert(
            "alpha".to_string(),
            SubprocessPlugin {
                path: std::path::PathBuf::from("/fake/meta-alpha"),
                info: PluginInfo {
                    name: "alpha".to_string(),
                    version: "2.0.0".to_string(),
                    commands: vec![],
                    description: Some("A plugin".to_string()),
                    help: None,
                },
            },
        );

        let plugins = manager.list_plugins();
        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[0].0, "alpha"); // Should be sorted alphabetically
        assert_eq!(plugins[1].0, "zebra");
    }

    #[test]
    fn test_plugin_help_serialization() {
        let mut commands = std::collections::HashMap::new();
        commands.insert("cmd1".to_string(), "Description 1".to_string());

        let help = PluginHelp {
            usage: "test usage".to_string(),
            commands,
            examples: vec!["example 1".to_string()],
            note: Some("a note".to_string()),
        };

        let json = serde_json::to_string(&help).unwrap();
        assert!(json.contains("\"usage\":\"test usage\""));
        assert!(json.contains("\"examples\":[\"example 1\"]"));

        // Deserialize back
        let parsed: PluginHelp = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.usage, "test usage");
        assert_eq!(parsed.examples, vec!["example 1"]);
        assert_eq!(parsed.note, Some("a note".to_string()));
    }

    #[test]
    fn test_plugin_help_deserialization_with_defaults() {
        // Test that missing optional fields use defaults
        let json = r#"{"usage": "test"}"#;
        let help: PluginHelp = serde_json::from_str(json).unwrap();
        assert_eq!(help.usage, "test");
        assert!(help.commands.is_empty());
        assert!(help.examples.is_empty());
        assert!(help.note.is_none());
    }
}
