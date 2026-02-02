//! GitHub-based plugin registry for meta.
//!
//! The registry is a GitHub repository containing plugin metadata files.
//! Plugin authors submit PRs to register their plugins, and users can
//! install plugins directly from the registry.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

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
        Ok(meta_core::meta_dir().join("config.yaml"))
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

/// Supported archive formats for plugin distribution
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArchiveFormat {
    TarGz,
    Zip,
}

impl ArchiveFormat {
    /// Detect archive format from URL or filename
    pub fn from_url(url: &str) -> Option<Self> {
        if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
            Some(Self::TarGz)
        } else if url.ends_with(".zip") {
            Some(Self::Zip)
        } else {
            None
        }
    }
}

/// Make a file executable on Unix systems (chmod 755)
#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
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

    /// Get the default plugins directory (~/.meta/plugins/)
    fn default_plugins_dir() -> Result<PathBuf> {
        meta_core::data_dir::data_subdir("plugins")
    }

    /// Ensure the plugins directory exists
    fn ensure_plugins_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.plugins_dir).with_context(|| {
            format!(
                "Failed to create plugins directory: {}",
                self.plugins_dir.display()
            )
        })
    }

    /// Download bytes from a URL
    fn download(&self, url: &str) -> Result<Vec<u8>> {
        let response = ureq::get(url)
            .call()
            .with_context(|| format!("Failed to download {url}"))?;

        let mut bytes = Vec::new();
        response
            .into_reader()
            .read_to_end(&mut bytes)
            .with_context(|| "Failed to read download")?;

        Ok(bytes)
    }

    /// Install a plugin from the registry
    pub fn install(&self, metadata: &PluginMetadata) -> Result<Vec<String>> {
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

        self.ensure_plugins_dir()?;
        let bytes = self.download(&download_url)?;

        // Extract and validate
        let installed = self.extract_archive(&download_url, &bytes)?;
        self.validate_installed(&installed)?;

        if self.verbose {
            println!(
                "Successfully installed {} v{}",
                metadata.name, metadata.version
            );
        }

        Ok(installed)
    }

    /// Install a plugin directly from a URL (bypasses registry)
    ///
    /// Downloads the archive, extracts it, and validates the plugin
    /// by running `--meta-plugin-info` on the extracted binary.
    pub fn install_from_url(&self, url: &str) -> Result<String> {
        if self.verbose {
            println!("Downloading from: {url}");
        }

        self.ensure_plugins_dir()?;
        let bytes = self.download(url)?;

        // Extract the archive and collect installed plugin names
        let installed = self.extract_archive(url, &bytes)?;
        self.validate_installed(&installed)?;

        let primary_plugin = installed.first().unwrap().clone();
        if self.verbose {
            println!("Successfully installed: {}", installed.join(", "));
        }

        Ok(primary_plugin)
    }

    /// Validate a list of installed plugins
    fn validate_installed(&self, installed: &[String]) -> Result<()> {
        if installed.is_empty() {
            anyhow::bail!("No meta-* executables found in archive");
        }

        for plugin_name in installed {
            let plugin_path = self.plugins_dir.join(plugin_name);
            self.validate_plugin(&plugin_path).with_context(|| {
                // Remove invalid plugin on failure
                let _ = std::fs::remove_file(&plugin_path);
                format!("Plugin validation failed for {plugin_name}")
            })?;
        }

        Ok(())
    }

    /// Extract archive and return list of installed plugin names
    fn extract_archive(&self, url: &str, bytes: &[u8]) -> Result<Vec<String>> {
        let format = ArchiveFormat::from_url(url)
            .with_context(|| format!("Unsupported archive format: {url}"))?;

        match format {
            ArchiveFormat::TarGz => self.extract_tar_gz(bytes),
            ArchiveFormat::Zip => self.extract_zip(bytes),
        }
    }

    /// Extract a tar.gz archive
    fn extract_tar_gz(&self, bytes: &[u8]) -> Result<Vec<String>> {
        let mut installed = Vec::new();
        let decoder = flate2::read::GzDecoder::new(bytes);
        let mut archive = tar::Archive::new(decoder);

        for entry in archive.entries()? {
            let mut entry = entry?;
            let file_name = entry
                .path()?
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string());

            if let Some(name) = file_name {
                if name.starts_with("meta-") {
                    let dest = self.plugins_dir.join(&name);
                    entry.unpack(&dest)?;
                    make_executable(&dest)?;
                    installed.push(name);
                }
            }
        }

        Ok(installed)
    }

    /// Extract a zip archive
    fn extract_zip(&self, bytes: &[u8]) -> Result<Vec<String>> {
        let mut installed = Vec::new();
        let cursor = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor)?;

        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            let file_name = file.name().to_string();

            if file_name.starts_with("meta-") {
                let dest = self.plugins_dir.join(&file_name);
                let mut dest_file = std::fs::File::create(&dest)?;
                std::io::copy(&mut file, &mut dest_file)?;
                make_executable(&dest)?;
                installed.push(file_name);
            }
        }

        Ok(installed)
    }

    /// Validate a plugin by running --meta-plugin-info
    fn validate_plugin(&self, plugin_path: &Path) -> Result<()> {
        use std::process::Command;

        let output = Command::new(plugin_path)
            .arg("--meta-plugin-info")
            .output()
            .with_context(|| format!("Failed to execute {}", plugin_path.display()))?;

        if !output.status.success() {
            anyhow::bail!(
                "Plugin did not respond to --meta-plugin-info (exit code: {:?})",
                output.status.code()
            );
        }

        // Try to parse the output as JSON to verify it's valid
        let stdout = String::from_utf8_lossy(&output.stdout);
        let _: serde_json::Value = serde_json::from_str(&stdout)
            .with_context(|| "Plugin --meta-plugin-info output is not valid JSON")?;

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

    #[test]
    fn test_archive_format_from_url() {
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/plugin.tar.gz"),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/plugin.tgz"),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/plugin.zip"),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            ArchiveFormat::from_url("https://example.com/plugin.exe"),
            None
        );
        assert_eq!(ArchiveFormat::from_url("https://example.com/plugin"), None);
    }

    #[test]
    fn test_extract_tar_gz() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let installer = PluginInstaller {
            plugins_dir: dir.path().to_path_buf(),
            verbose: false,
        };

        // Create a minimal tar.gz with a meta-test file
        let mut builder = tar::Builder::new(Vec::new());

        let content = b"#!/bin/sh\necho test";
        let mut header = tar::Header::new_gnu();
        header.set_path("meta-test").unwrap();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, &content[..]).unwrap();

        let tar_data = builder.into_inner().unwrap();

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&tar_data).unwrap();
        let gz_data = encoder.finish().unwrap();

        let installed = installer.extract_tar_gz(&gz_data).unwrap();

        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0], "meta-test");
        assert!(dir.path().join("meta-test").exists());
    }

    #[test]
    fn test_extract_tar_gz_filters_non_meta_files() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let installer = PluginInstaller {
            plugins_dir: dir.path().to_path_buf(),
            verbose: false,
        };

        // Create a tar.gz with both meta-* and non-meta files
        let mut builder = tar::Builder::new(Vec::new());

        for name in &["meta-plugin", "readme.txt", "other-binary"] {
            let content = b"content";
            let mut header = tar::Header::new_gnu();
            header.set_path(name).unwrap();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, &content[..]).unwrap();
        }

        let tar_data = builder.into_inner().unwrap();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&tar_data).unwrap();
        let gz_data = encoder.finish().unwrap();

        let installed = installer.extract_tar_gz(&gz_data).unwrap();

        // Only meta-plugin should be extracted
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0], "meta-plugin");
        assert!(dir.path().join("meta-plugin").exists());
        assert!(!dir.path().join("readme.txt").exists());
        assert!(!dir.path().join("other-binary").exists());
    }

    #[test]
    fn test_extract_zip() {
        use std::io::Write;
        use zip::write::FileOptions;

        let dir = tempfile::tempdir().unwrap();
        let installer = PluginInstaller {
            plugins_dir: dir.path().to_path_buf(),
            verbose: false,
        };

        // Create a minimal zip with a meta-test file
        let mut zip_data = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
            let options = FileOptions::default();
            zip.start_file("meta-test", options).unwrap();
            zip.write_all(b"#!/bin/sh\necho test").unwrap();
            zip.finish().unwrap();
        }

        let installed = installer.extract_zip(&zip_data).unwrap();

        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0], "meta-test");
        assert!(dir.path().join("meta-test").exists());
    }

    #[test]
    fn test_extract_zip_filters_non_meta_files() {
        use std::io::Write;
        use zip::write::FileOptions;

        let dir = tempfile::tempdir().unwrap();
        let installer = PluginInstaller {
            plugins_dir: dir.path().to_path_buf(),
            verbose: false,
        };

        // Create a zip with both meta-* and non-meta files
        let mut zip_data = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_data));
            let options = FileOptions::default();

            for name in &["meta-plugin", "readme.txt", "other-binary"] {
                zip.start_file(*name, options).unwrap();
                zip.write_all(b"content").unwrap();
            }
            zip.finish().unwrap();
        }

        let installed = installer.extract_zip(&zip_data).unwrap();

        // Only meta-plugin should be extracted
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0], "meta-plugin");
        assert!(dir.path().join("meta-plugin").exists());
        assert!(!dir.path().join("readme.txt").exists());
        assert!(!dir.path().join("other-binary").exists());
    }

    #[test]
    fn test_extract_archive_unsupported_format() {
        let dir = tempfile::tempdir().unwrap();
        let installer = PluginInstaller {
            plugins_dir: dir.path().to_path_buf(),
            verbose: false,
        };

        let result = installer.extract_archive("https://example.com/plugin.exe", &[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unsupported"));
    }
}
