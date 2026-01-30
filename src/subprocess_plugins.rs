//! Subprocess-based plugin system for meta.
//!
//! Plugins are standalone executables that communicate via JSON over stdin/stdout.
//! This approach provides better isolation, language flexibility, and simpler debugging.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[allow(unused_imports)]
pub use meta_plugin_protocol::{
    ExecutionPlan, PlannedCommand, PluginHelp, PluginInfo, PluginRequest, PluginRequestOptions,
    PlanResponse as PluginResponse,
};

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

        for plugin in self.plugins.values() {
            for plugin_cmd in &plugin.info.commands {
                // Check if the input command starts with this plugin command
                if command == plugin_cmd || command.starts_with(&format!("{} ", plugin_cmd)) {
                    return true;
                }
                // Also check single-word match for fallback
                if plugin_cmd == cmd_parts[0] {
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

        // Find the best (longest) matching command across all plugins
        let mut best_match: Option<(&SubprocessPlugin, &str)> = None;
        let mut best_match_len = 0;

        for plugin in self.plugins.values() {
            for plugin_cmd in &plugin.info.commands {
                // Check if the input command starts with this plugin command
                if command == plugin_cmd || command.starts_with(&format!("{} ", plugin_cmd)) {
                    let cmd_len = plugin_cmd.split_whitespace().count();
                    if cmd_len > best_match_len {
                        best_match = Some((plugin, plugin_cmd));
                        best_match_len = cmd_len;
                    }
                }
                // Also check if the first word of plugin command matches first word of input
                // This allows "project blablabla" to route to the project plugin
                else if best_match_len == 0 {
                    let plugin_cmd_first = plugin_cmd.split_whitespace().next().unwrap_or("");
                    if plugin_cmd_first == cmd_parts[0] {
                        // Use the full input command (plugin will handle unknown subcommand)
                        best_match = Some((plugin, command));
                        best_match_len = 1;
                    }
                }
            }
        }

        if let Some((plugin, matched_cmd)) = best_match {
            return self.execute_plugin(plugin, matched_cmd, args, projects, &options);
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
        // Extract the remaining args after the matched command
        // e.g., if command is "git snapshot create" and args is ["git", "snapshot", "create", "test-snapshot"]
        // then remaining_args should be ["test-snapshot"]
        let cmd_word_count = command.split_whitespace().count();
        let remaining_args: Vec<String> = args.iter().skip(cmd_word_count).cloned().collect();

        let request = PluginRequest {
            command: command.to_string(),
            args: remaining_args,
            projects: projects.to_vec(),
            cwd: std::env::current_dir()?.to_string_lossy().to_string(),
            options: options.clone(),
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
            .stdout(Stdio::piped()) // Capture stdout to parse response
            .stderr(Stdio::inherit()) // Let stderr pass through for error messages
            .spawn()
            .with_context(|| format!("Failed to execute plugin {}", plugin.path.display()))?;

        // Send request to plugin's stdin
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(request_json.as_bytes())?;
        }

        let output = child.wait_with_output()?;

        if !output.status.success() {
            // Plugin already printed its error to stderr, just propagate the exit code
            std::process::exit(output.status.code().unwrap_or(1));
        }

        // Try to parse the response as JSON
        let stdout_str = String::from_utf8_lossy(&output.stdout);

        // If stdout is empty, plugin handled execution silently
        if stdout_str.trim().is_empty() {
            return Ok(true);
        }

        // If stdout doesn't look like JSON, print it (legacy plugin output)
        if !stdout_str.trim().starts_with('{') {
            print!("{}", stdout_str);
            return Ok(true);
        }

        // Parse the plugin response
        match serde_json::from_str::<PluginResponse>(&stdout_str) {
            Ok(response) => {
                // Plugin returned an execution plan - execute it via loop_lib
                self.execute_plan(&response.plan, options)
            }
            Err(_) => {
                // Couldn't parse as our protocol - print output as-is (legacy behavior)
                print!("{}", stdout_str);
                Ok(true)
            }
        }
    }

    /// Execute an execution plan via loop_lib
    fn execute_plan(&self, plan: &ExecutionPlan, options: &PluginRequestOptions) -> Result<bool> {
        use loop_lib::{DirCommand, LoopConfig, run_commands};

        if plan.commands.is_empty() {
            return Ok(true); // Nothing to execute
        }

        // Convert plan commands to loop_lib DirCommand format
        let commands: Vec<DirCommand> = plan.commands
            .iter()
            .map(|c| DirCommand {
                dir: c.dir.clone(),
                cmd: c.cmd.clone(),
                env: c.env.clone(),
            })
            .collect();

        let config = LoopConfig {
            directories: vec![], // Not used by run_commands
            ignore: vec![],
            verbose: options.verbose,
            silent: options.silent,
            add_aliases_to_global_looprc: false,
            include_filters: options.include_filters.clone(),
            exclude_filters: options.exclude_filters.clone(),
            parallel: plan.parallel.unwrap_or(options.parallel),
            dry_run: options.dry_run,
            json_output: options.json_output,
            spawn_stagger_ms: 0,
            env: None, // Per-command env is set on individual DirCommands
        };

        run_commands(&config, &commands)?;
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
                dry_run: false,
                ..Default::default()
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
        assert!(!options.dry_run);
        assert!(!options.silent);
        assert!(options.include_filters.is_none());
        assert!(options.exclude_filters.is_none());
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

    // ============ PluginResponse Parsing Tests ============

    #[test]
    fn test_plugin_response_basic() {
        let json = r#"{
            "plan": {
                "commands": [
                    {"dir": "./repo1", "cmd": "git status"},
                    {"dir": "./repo2", "cmd": "git status"}
                ]
            }
        }"#;
        let response: PluginResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.plan.commands.len(), 2);
        assert_eq!(response.plan.commands[0].dir, "./repo1");
        assert_eq!(response.plan.commands[0].cmd, "git status");
        assert_eq!(response.plan.commands[1].dir, "./repo2");
        assert_eq!(response.plan.commands[1].cmd, "git status");
        assert!(response.plan.parallel.is_none());
    }

    #[test]
    fn test_plugin_response_with_parallel() {
        let json = r#"{
            "plan": {
                "commands": [
                    {"dir": "/abs/path", "cmd": "npm install"}
                ],
                "parallel": true
            }
        }"#;
        let response: PluginResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.plan.commands.len(), 1);
        assert_eq!(response.plan.parallel, Some(true));
    }

    #[test]
    fn test_plugin_response_parallel_false() {
        let json = r#"{
            "plan": {
                "commands": [
                    {"dir": ".", "cmd": "echo hello"}
                ],
                "parallel": false
            }
        }"#;
        let response: PluginResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.plan.parallel, Some(false));
    }

    #[test]
    fn test_plugin_response_empty_commands() {
        let json = r#"{
            "plan": {
                "commands": []
            }
        }"#;
        let response: PluginResponse = serde_json::from_str(json).unwrap();
        assert!(response.plan.commands.is_empty());
    }

    // ============ ExecutionPlan Tests ============

    #[test]
    fn test_execution_plan_deserialization() {
        let json = r#"{
            "commands": [
                {"dir": "proj1", "cmd": "make build"},
                {"dir": "proj2", "cmd": "cargo build"}
            ],
            "parallel": true
        }"#;
        let plan: ExecutionPlan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.commands.len(), 2);
        assert_eq!(plan.commands[0].dir, "proj1");
        assert_eq!(plan.commands[0].cmd, "make build");
        assert_eq!(plan.commands[1].dir, "proj2");
        assert_eq!(plan.commands[1].cmd, "cargo build");
        assert_eq!(plan.parallel, Some(true));
    }

    #[test]
    fn test_execution_plan_no_parallel_field() {
        let json = r#"{
            "commands": [
                {"dir": ".", "cmd": "ls"}
            ]
        }"#;
        let plan: ExecutionPlan = serde_json::from_str(json).unwrap();
        assert!(plan.parallel.is_none());
    }

    // ============ PlannedCommand Tests ============

    #[test]
    fn test_planned_command_deserialization() {
        let json = r#"{"dir": "/home/user/project", "cmd": "git pull --rebase"}"#;
        let cmd: PlannedCommand = serde_json::from_str(json).unwrap();
        assert_eq!(cmd.dir, "/home/user/project");
        assert_eq!(cmd.cmd, "git pull --rebase");
    }

    #[test]
    fn test_planned_command_relative_dir() {
        let json = r#"{"dir": "./relative/path", "cmd": "npm test"}"#;
        let cmd: PlannedCommand = serde_json::from_str(json).unwrap();
        assert_eq!(cmd.dir, "./relative/path");
        assert_eq!(cmd.cmd, "npm test");
    }

    #[test]
    fn test_planned_command_complex_command() {
        let json = r#"{"dir": ".", "cmd": "git clone https://github.com/org/repo.git --depth 1"}"#;
        let cmd: PlannedCommand = serde_json::from_str(json).unwrap();
        assert_eq!(cmd.cmd, "git clone https://github.com/org/repo.git --depth 1");
    }

    // ============ PluginRequest with dry_run Tests ============

    #[test]
    fn test_plugin_request_with_dry_run() {
        let request = PluginRequest {
            command: "git status".to_string(),
            args: vec![],
            projects: vec!["proj1".to_string()],
            cwd: "/workspace".to_string(),
            options: PluginRequestOptions {
                json_output: false,
                verbose: false,
                parallel: false,
                dry_run: true,
                ..Default::default()
            },
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"dry_run\":true"));
    }

    #[test]
    fn test_plugin_request_all_options_enabled() {
        let request = PluginRequest {
            command: "build".to_string(),
            args: vec!["--release".to_string()],
            projects: vec![],
            cwd: ".".to_string(),
            options: PluginRequestOptions {
                json_output: true,
                verbose: true,
                parallel: true,
                dry_run: true,
                silent: true,
                recursive: true,
                depth: Some(3),
                include_filters: Some(vec!["frontend".to_string()]),
                exclude_filters: Some(vec!["tests".to_string()]),
                strict: false,
            },
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"json_output\":true"));
        assert!(json.contains("\"verbose\":true"));
        assert!(json.contains("\"parallel\":true"));
        assert!(json.contains("\"dry_run\":true"));
        assert!(json.contains("\"silent\":true"));
        assert!(json.contains("\"include_filters\""));
        assert!(json.contains("\"exclude_filters\""));
    }

    // ============ Edge Cases and Error Handling ============

    #[test]
    fn test_plugin_response_invalid_json() {
        // Invalid JSON should fail to parse
        let json = r#"{"plan": "not an object"}"#;
        let result: Result<PluginResponse, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_plugin_response_missing_plan() {
        // Missing plan field should fail
        let json = r#"{"commands": []}"#;
        let result: Result<PluginResponse, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_execution_plan_multiple_different_commands() {
        // Test a realistic execution plan with different commands per directory
        let json = r#"{
            "commands": [
                {"dir": "./frontend", "cmd": "npm install"},
                {"dir": "./backend", "cmd": "cargo build"},
                {"dir": "./scripts", "cmd": "python setup.py install"},
                {"dir": "./docs", "cmd": "make html"}
            ],
            "parallel": false
        }"#;
        let plan: ExecutionPlan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.commands.len(), 4);
        assert_eq!(plan.commands[0].cmd, "npm install");
        assert_eq!(plan.commands[1].cmd, "cargo build");
        assert_eq!(plan.commands[2].cmd, "python setup.py install");
        assert_eq!(plan.commands[3].cmd, "make html");
    }

    #[test]
    fn test_planned_command_with_special_characters() {
        let json = r#"{"dir": "./path with spaces", "cmd": "echo \"hello world\" && echo 'single quotes'"}"#;
        let cmd: PlannedCommand = serde_json::from_str(json).unwrap();
        assert_eq!(cmd.dir, "./path with spaces");
        assert_eq!(cmd.cmd, "echo \"hello world\" && echo 'single quotes'");
    }

    // ============ get_plugin_for_command Tests ============

    #[test]
    fn test_get_plugin_for_command_found() {
        let mut manager = SubprocessPluginManager::new();
        manager.plugins.insert(
            "git".to_string(),
            SubprocessPlugin {
                path: std::path::PathBuf::from("/fake/meta-git"),
                info: PluginInfo {
                    name: "git".to_string(),
                    version: "1.0.0".to_string(),
                    commands: vec!["git status".to_string()],
                    description: None,
                    help: None,
                },
            },
        );

        let plugin = manager.get_plugin_for_command("git status");
        assert!(plugin.is_some());
        assert_eq!(plugin.unwrap().info.name, "git");
    }

    #[test]
    fn test_get_plugin_for_command_not_found() {
        let manager = SubprocessPluginManager::new();
        let plugin = manager.get_plugin_for_command("unknown command");
        assert!(plugin.is_none());
    }

    #[test]
    fn test_get_plugin_for_command_empty() {
        let manager = SubprocessPluginManager::new();
        let plugin = manager.get_plugin_for_command("");
        assert!(plugin.is_none());
    }

    // ============ get_plugin Tests ============

    #[test]
    fn test_get_plugin_found() {
        let mut manager = SubprocessPluginManager::new();
        manager.plugins.insert(
            "test".to_string(),
            SubprocessPlugin {
                path: std::path::PathBuf::from("/fake/meta-test"),
                info: PluginInfo {
                    name: "test".to_string(),
                    version: "2.0.0".to_string(),
                    commands: vec![],
                    description: Some("Test plugin".to_string()),
                    help: None,
                },
            },
        );

        let plugin = manager.get_plugin("test");
        assert!(plugin.is_some());
        assert_eq!(plugin.unwrap().info.version, "2.0.0");
    }

    #[test]
    fn test_get_plugin_not_found() {
        let manager = SubprocessPluginManager::new();
        let plugin = manager.get_plugin("nonexistent");
        assert!(plugin.is_none());
    }
}
