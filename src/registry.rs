//! GitHub-based plugin registry for meta.
//!
//! The registry is a GitHub repository containing plugin metadata files.
//! Plugin authors submit PRs to register their plugins, and users can
//! install plugins directly from the registry.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Default registry URL
pub const DEFAULT_REGISTRY: &str = "https://raw.githubusercontent.com/anthropics/meta-plugins/main";

/// Plugin metadata from the registry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMetadata {
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: String,
    pub repository: String,
    #[serde(default)]
    pub releases: HashMap<String, PlatformReleases>,
    #[serde(default)]
    pub checksum: Option<String>,
}

/// Platform-specific release URLs
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlatformReleases {
    #[serde(rename = "darwin-arm64")]
    pub darwin_arm64: Option<String>,
    #[serde(rename = "darwin-x64")]
    pub darwin_x64: Option<String>,
    #[serde(rename = "linux-x64")]
    pub linux_x64: Option<String>,
    #[serde(rename = "linux-arm64")]
    pub linux_arm64: Option<String>,
    #[serde(rename = "windows-x64")]
    pub windows_x64: Option<String>,
}

/// Registry index containing all available plugins
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RegistryIndex {
    pub plugins: HashMap<String, PluginIndexEntry>,
}

/// Summary entry in the registry index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginIndexEntry {
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: String,
}

/// Registry configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RegistryConfig {
    #[serde(default)]
    pub registries: Vec<String>,
}

impl RegistryConfig {
    /// Load registry config from ~/.meta/config.yaml
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)
                .with_context(|| format!("Failed to read {}", config_path.display()))?;
            let config: RegistryConfig = serde_yaml::from_str(&content)
                .with_context(|| "Failed to parse registry config")?;
            Ok(config)
        } else {
            Ok(Self::default())
        }
    }

    /// Get the config file path
    fn config_path() -> Result<PathBuf> {
        let home = std::env::var("HOME").context("HOME environment variable not set")?;
        Ok(PathBuf::from(home).join(".meta").join("config.yaml"))
    }

    /// Get list of registries (defaults to the public registry)
    pub fn get_registries(&self) -> Vec<String> {
        if self.registries.is_empty() {
            vec![DEFAULT_REGISTRY.to_string()]
        } else {
            self.registries.clone()
        }
    }
}

/// Plugin registry client
pub struct RegistryClient {
    registries: Vec<String>,
    verbose: bool,
}

impl RegistryClient {
    /// Create a new registry client
    pub fn new(verbose: bool) -> Result<Self> {
        let config = RegistryConfig::load().unwrap_or_default();
        Ok(Self {
            registries: config.get_registries(),
            verbose,
        })
    }

    /// Create a new registry client with custom registries
    #[allow(dead_code)]
    pub fn with_registries(registries: Vec<String>, verbose: bool) -> Self {
        Self {
            registries,
            verbose,
        }
    }

    /// Fetch the registry index
    pub fn fetch_index(&self) -> Result<RegistryIndex> {
        let mut combined_index = RegistryIndex::default();

        for registry_url in &self.registries {
            let index_url = format!("{registry_url}/plugins/index.json");
            if self.verbose {
                println!("Fetching registry index from: {index_url}");
            }

            match self.fetch_json::<RegistryIndex>(&index_url) {
                Ok(index) => {
                    // Merge plugins (later registries override earlier ones)
                    combined_index.plugins.extend(index.plugins);
                }
                Err(e) => {
                    if self.verbose {
                        eprintln!("Warning: Failed to fetch from {registry_url}: {e}");
                    }
                }
            }
        }

        Ok(combined_index)
    }

    /// Fetch plugin metadata
    pub fn fetch_plugin_metadata(&self, name: &str) -> Result<PluginMetadata> {
        for registry_url in &self.registries {
            let plugin_url = format!("{registry_url}/plugins/{name}/plugin.json");
            if self.verbose {
                println!("Fetching plugin metadata from: {plugin_url}");
            }

            match self.fetch_json::<PluginMetadata>(&plugin_url) {
                Ok(metadata) => return Ok(metadata),
                Err(e) => {
                    if self.verbose {
                        eprintln!("Warning: Plugin {name} not found in {registry_url}: {e}");
                    }
                }
            }
        }

        anyhow::bail!("Plugin '{name}' not found in any registry")
    }

    /// Search for plugins matching a query
    pub fn search(&self, query: &str) -> Result<Vec<PluginIndexEntry>> {
        let index = self.fetch_index()?;
        let query_lower = query.to_lowercase();

        let results: Vec<PluginIndexEntry> = index
            .plugins
            .into_values()
            .filter(|p| {
                p.name.to_lowercase().contains(&query_lower)
                    || p.description.to_lowercase().contains(&query_lower)
            })
            .collect();

        Ok(results)
    }

    /// Fetch JSON from a URL
    fn fetch_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let response = ureq::get(url)
            .call()
            .with_context(|| format!("Failed to fetch {url}"))?;

        let body = response
            .into_string()
            .with_context(|| "Failed to read response body")?;

        serde_json::from_str(&body).with_context(|| "Failed to parse JSON response")
    }

    /// Get the current platform identifier
    pub fn current_platform() -> &'static str {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        return "darwin-arm64";
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        return "darwin-x64";
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        return "linux-x64";
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        return "linux-arm64";
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        return "windows-x64";
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64"),
            all(target_os = "windows", target_arch = "x86_64"),
        )))]
        return "unknown";
    }
}

/// Plugin installer
pub struct PluginInstaller {
    plugins_dir: PathBuf,
    verbose: bool,
}

impl PluginInstaller {
    /// Create a new plugin installer
    pub fn new(verbose: bool) -> Result<Self> {
        let plugins_dir = Self::default_plugins_dir()?;
        Ok(Self {
            plugins_dir,
            verbose,
        })
    }

    /// Get the default plugins directory
    fn default_plugins_dir() -> Result<PathBuf> {
        let home = std::env::var("HOME").context("HOME environment variable not set")?;
        Ok(PathBuf::from(home).join(".meta-plugins"))
    }

    /// Install a plugin from the registry
    pub fn install(&self, metadata: &PluginMetadata) -> Result<()> {
        let platform = RegistryClient::current_platform();

        // Get the download URL for the current platform and latest version
        let releases = metadata
            .releases
            .get(&metadata.version)
            .with_context(|| format!("No releases found for version {}", metadata.version))?;

        let download_url = self
            .get_platform_url(releases, platform)
            .with_context(|| format!("No release available for platform {platform}"))?;

        if self.verbose {
            println!(
                "Downloading {} v{} for {}",
                metadata.name, metadata.version, platform
            );
            println!("URL: {download_url}");
        }

        // Create plugins directory if it doesn't exist
        std::fs::create_dir_all(&self.plugins_dir).with_context(|| {
            format!(
                "Failed to create plugins directory: {}",
                self.plugins_dir.display()
            )
        })?;

        // Download the archive
        let response = ureq::get(&download_url)
            .call()
            .with_context(|| format!("Failed to download {download_url}"))?;

        let mut bytes = Vec::new();
        response
            .into_reader()
            .read_to_end(&mut bytes)
            .with_context(|| "Failed to read download")?;

        // Extract the archive
        self.extract_archive(&bytes, &download_url, &metadata.name)?;

        if self.verbose {
            println!(
                "Successfully installed {} v{}",
                metadata.name, metadata.version
            );
        }

        Ok(())
    }

    /// Get the download URL for a specific platform
    fn get_platform_url(&self, releases: &PlatformReleases, platform: &str) -> Option<String> {
        match platform {
            "darwin-arm64" => releases.darwin_arm64.clone(),
            "darwin-x64" => releases.darwin_x64.clone(),
            "linux-x64" => releases.linux_x64.clone(),
            "linux-arm64" => releases.linux_arm64.clone(),
            "windows-x64" => releases.windows_x64.clone(),
            _ => None,
        }
    }

    /// Extract an archive to the plugins directory
    fn extract_archive(&self, bytes: &[u8], url: &str, _plugin_name: &str) -> Result<()> {
        if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
            let decoder = flate2::read::GzDecoder::new(bytes);
            let mut archive = tar::Archive::new(decoder);

            for entry in archive.entries()? {
                let mut entry = entry?;
                let path = entry.path()?;
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

                // Only extract executables (meta-* files)
                if file_name.starts_with("meta-") {
                    let dest = self.plugins_dir.join(file_name);
                    entry.unpack(&dest)?;

                    // Make executable on Unix
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let mut perms = std::fs::metadata(&dest)?.permissions();
                        perms.set_mode(0o755);
                        std::fs::set_permissions(&dest, perms)?;
                    }
                }
            }
        } else if url.ends_with(".zip") {
            let cursor = std::io::Cursor::new(bytes);
            let mut archive = zip::ZipArchive::new(cursor)?;

            for i in 0..archive.len() {
                let mut file = archive.by_index(i)?;
                let file_name = file.name();

                if file_name.starts_with("meta-") {
                    let dest = self.plugins_dir.join(file_name);
                    let mut dest_file = std::fs::File::create(&dest)?;
                    std::io::copy(&mut file, &mut dest_file)?;

                    // Make executable on Unix
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let mut perms = std::fs::metadata(&dest)?.permissions();
                        perms.set_mode(0o755);
                        std::fs::set_permissions(&dest, perms)?;
                    }
                }
            }
        } else {
            anyhow::bail!("Unsupported archive format: {url}");
        }

        Ok(())
    }

    /// List installed plugins
    pub fn list_installed(&self) -> Result<Vec<String>> {
        let mut plugins = Vec::new();

        if self.plugins_dir.exists() {
            for entry in std::fs::read_dir(&self.plugins_dir)? {
                let entry = entry?;
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("meta-")
                        && !name.ends_with(".dylib")
                        && !name.ends_with(".so")
                    {
                        plugins.push(name.to_string());
                    }
                }
            }
        }

        Ok(plugins)
    }

    /// Uninstall a plugin
    pub fn uninstall(&self, name: &str) -> Result<()> {
        let plugin_name = if name.starts_with("meta-") {
            name.to_string()
        } else {
            format!("meta-{name}")
        };

        let plugin_path = self.plugins_dir.join(&plugin_name);
        if plugin_path.exists() {
            std::fs::remove_file(&plugin_path)
                .with_context(|| format!("Failed to remove {}", plugin_path.display()))?;
            if self.verbose {
                println!("Uninstalled {plugin_name}");
            }
            Ok(())
        } else {
            anyhow::bail!("Plugin {plugin_name} is not installed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_config_default() {
        let config = RegistryConfig::default();
        let registries = config.get_registries();
        assert_eq!(registries.len(), 1);
        assert!(registries[0].contains("meta-plugins"));
    }

    #[test]
    fn test_current_platform() {
        let platform = RegistryClient::current_platform();
        assert!(!platform.is_empty());
        // Platform should be one of the known values
        let known_platforms = [
            "darwin-arm64",
            "darwin-x64",
            "linux-x64",
            "linux-arm64",
            "windows-x64",
            "unknown",
        ];
        assert!(known_platforms.contains(&platform));
    }

    #[test]
    fn test_plugin_metadata_serialization() {
        let metadata = PluginMetadata {
            name: "docker".to_string(),
            description: "Docker commands for meta".to_string(),
            version: "1.0.0".to_string(),
            author: "testuser".to_string(),
            repository: "github.com/testuser/meta-plugin-docker".to_string(),
            releases: HashMap::new(),
            checksum: Some("sha256:abc123".to_string()),
        };

        let json = serde_json::to_string(&metadata).unwrap();
        assert!(json.contains("\"name\":\"docker\""));
        assert!(json.contains("\"version\":\"1.0.0\""));

        // Deserialize
        let parsed: PluginMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "docker");
        assert_eq!(parsed.author, "testuser");
    }

    #[test]
    fn test_platform_releases_serialization() {
        let releases = PlatformReleases {
            darwin_arm64: Some("https://example.com/darwin-arm64.tar.gz".to_string()),
            darwin_x64: Some("https://example.com/darwin-x64.tar.gz".to_string()),
            linux_x64: Some("https://example.com/linux-x64.tar.gz".to_string()),
            linux_arm64: None,
            windows_x64: None,
        };

        let json = serde_json::to_string(&releases).unwrap();
        assert!(json.contains("darwin-arm64"));
        assert!(json.contains("darwin-x64"));
    }

    #[test]
    fn test_registry_index_entry_serialization() {
        let entry = PluginIndexEntry {
            name: "npm".to_string(),
            description: "NPM commands for meta".to_string(),
            version: "2.0.0".to_string(),
            author: "npmuser".to_string(),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: PluginIndexEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "npm");
        assert_eq!(parsed.version, "2.0.0");
    }

    #[test]
    fn test_registry_config_custom_registries() {
        let config = RegistryConfig {
            registries: vec![
                "https://custom.registry.com".to_string(),
                "https://another.registry.com".to_string(),
            ],
        };

        let registries = config.get_registries();
        assert_eq!(registries.len(), 2);
        assert_eq!(registries[0], "https://custom.registry.com");
        assert_eq!(registries[1], "https://another.registry.com");
    }

    #[test]
    fn test_plugin_installer_list_installed_empty() {
        let dir = tempfile::tempdir().unwrap();
        let installer = PluginInstaller {
            plugins_dir: dir.path().to_path_buf(),
            verbose: false,
        };

        let plugins = installer.list_installed().unwrap();
        assert!(plugins.is_empty());
    }

    #[test]
    fn test_plugin_installer_list_installed_with_plugins() {
        let dir = tempfile::tempdir().unwrap();

        // Create some fake plugin files
        std::fs::write(dir.path().join("meta-docker"), "fake binary").unwrap();
        std::fs::write(dir.path().join("meta-npm"), "fake binary").unwrap();
        std::fs::write(dir.path().join("other-file"), "not a plugin").unwrap();
        std::fs::write(dir.path().join("meta-old.dylib"), "dylib file").unwrap();

        let installer = PluginInstaller {
            plugins_dir: dir.path().to_path_buf(),
            verbose: false,
        };

        let plugins = installer.list_installed().unwrap();
        assert_eq!(plugins.len(), 2);
        assert!(plugins.contains(&"meta-docker".to_string()));
        assert!(plugins.contains(&"meta-npm".to_string()));
        // Should not include non-meta files or dylibs
        assert!(!plugins.contains(&"other-file".to_string()));
        assert!(!plugins.contains(&"meta-old.dylib".to_string()));
    }

    #[test]
    fn test_plugin_installer_uninstall() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("meta-test"), "fake binary").unwrap();

        let installer = PluginInstaller {
            plugins_dir: dir.path().to_path_buf(),
            verbose: false,
        };

        // Uninstall should succeed
        installer.uninstall("test").unwrap();
        assert!(!dir.path().join("meta-test").exists());
    }

    #[test]
    fn test_plugin_installer_uninstall_not_installed() {
        let dir = tempfile::tempdir().unwrap();

        let installer = PluginInstaller {
            plugins_dir: dir.path().to_path_buf(),
            verbose: false,
        };

        // Uninstall should fail for non-existent plugin
        let result = installer.uninstall("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_registry_client_with_custom_registries() {
        let client =
            RegistryClient::with_registries(vec!["https://test.registry.com".to_string()], false);

        assert_eq!(client.registries.len(), 1);
        assert_eq!(client.registries[0], "https://test.registry.com");
    }
}
