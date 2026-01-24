//! Shared configuration types and parsing for .meta files.
//!
//! This module provides the core types and functions for finding and parsing
//! .meta configuration files (JSON and YAML formats).

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Represents a project entry in the .meta config.
/// Can be either a simple git URL string or an extended object with optional fields.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ProjectEntry {
    /// Simple format: just a git URL string
    Simple(String),
    /// Extended format: object with repo, optional path, and optional tags
    Extended {
        repo: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        tags: Vec<String>,
    },
}

/// Parsed project info after normalization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    pub path: String,
    pub repo: String,
    pub tags: Vec<String>,
}

/// The meta configuration file structure
#[derive(Debug, Deserialize, Default)]
pub struct MetaConfig {
    #[serde(default)]
    pub projects: HashMap<String, ProjectEntry>,
    #[serde(default)]
    pub ignore: Vec<String>,
}

/// Determines the format of a config file based on extension
#[derive(Clone)]
pub enum ConfigFormat {
    Json,
    Yaml,
}

/// Find the meta config file, checking for .meta, .meta.yaml, and .meta.yml
///
/// Walks up from `start_dir` to the filesystem root, looking for config files.
/// If `config_name` is provided, only looks for that specific filename.
pub fn find_meta_config(
    start_dir: &Path,
    config_name: Option<&PathBuf>,
) -> Option<(PathBuf, ConfigFormat)> {
    let candidates: Vec<(String, ConfigFormat)> = if let Some(name) = config_name {
        // User specified a config file name
        let name_str = name.to_string_lossy().to_string();
        if name_str.ends_with(".yaml") || name_str.ends_with(".yml") {
            vec![(name_str, ConfigFormat::Yaml)]
        } else {
            vec![(name_str, ConfigFormat::Json)]
        }
    } else {
        // Default: check all supported names
        vec![
            (".meta".to_string(), ConfigFormat::Json),
            (".meta.yaml".to_string(), ConfigFormat::Yaml),
            (".meta.yml".to_string(), ConfigFormat::Yaml),
        ]
    };

    let mut current_dir = start_dir.to_path_buf();
    loop {
        for (name, format) in &candidates {
            let candidate = current_dir.join(name);
            if candidate.exists() {
                return Some((candidate, format.clone()));
            }
        }
        if let Some(parent) = current_dir.parent() {
            current_dir = parent.to_path_buf();
        } else {
            return None;
        }
    }
}

/// Parse a meta config file (JSON or YAML) and return normalized project info and ignore list.
pub fn parse_meta_config(
    meta_path: &Path,
) -> anyhow::Result<(Vec<ProjectInfo>, Vec<String>)> {
    let config_str = std::fs::read_to_string(meta_path)
        .with_context(|| format!("Failed to read meta config file: '{}'", meta_path.display()))?;

    // Determine format from file extension
    let path_str = meta_path.to_string_lossy();
    let config: MetaConfig = if path_str.ends_with(".yaml") || path_str.ends_with(".yml") {
        serde_yaml::from_str(&config_str)
            .with_context(|| format!("Failed to parse YAML config file: {}", meta_path.display()))?
    } else {
        serde_json::from_str(&config_str)
            .with_context(|| format!("Failed to parse JSON config file: {}", meta_path.display()))?
    };

    // Convert project entries to normalized ProjectInfo
    let projects: Vec<ProjectInfo> = config
        .projects
        .into_iter()
        .map(|(name, entry)| {
            let (repo, path, tags) = match entry {
                ProjectEntry::Simple(repo) => (repo, name.clone(), vec![]),
                ProjectEntry::Extended { repo, path, tags } => {
                    let resolved_path = path.unwrap_or_else(|| name.clone());
                    (repo, resolved_path, tags)
                }
            };
            ProjectInfo {
                name,
                path,
                repo,
                tags,
            }
        })
        .collect();

    Ok((projects, config.ignore))
}

// ============================================================================
// Tree Walking
// ============================================================================

/// A node in the meta project tree, representing a project and its nested children.
#[derive(Debug, Clone, Serialize)]
pub struct MetaTreeNode {
    pub info: ProjectInfo,
    pub is_meta: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<MetaTreeNode>,
}

/// Walk a meta repository tree, discovering nested .meta repos.
///
/// Parses the .meta config at `start_dir` and for each project checks
/// if it has its own .meta file. Recursively expands children up to `max_depth`.
/// Uses cycle detection via path canonicalization.
///
/// `max_depth` of `None` means unlimited recursion.
/// `max_depth` of `Some(0)` means no recursion (only top-level projects).
pub fn walk_meta_tree(
    start_dir: &Path,
    max_depth: Option<usize>,
) -> anyhow::Result<Vec<MetaTreeNode>> {
    let (config_path, _format) = find_meta_config(start_dir, None)
        .ok_or_else(|| anyhow::anyhow!("No .meta config found in {}", start_dir.display()))?;

    let (projects, _ignore) = parse_meta_config(&config_path)?;
    let meta_dir = config_path.parent().unwrap_or(Path::new("."));

    let mut visited = std::collections::HashSet::new();
    visited.insert(meta_dir.canonicalize().unwrap_or(meta_dir.to_path_buf()));

    let depth = max_depth.unwrap_or(usize::MAX);
    Ok(walk_inner(meta_dir, &projects, depth, 0, &mut visited))
}

/// Flatten a meta tree into fully-qualified path strings.
///
/// For nested children, paths are joined with their parent
/// (e.g., a child with path "grandchild" under parent "child" becomes "child/grandchild").
pub fn flatten_meta_tree(nodes: &[MetaTreeNode]) -> Vec<String> {
    let mut paths = Vec::new();
    flatten_inner(nodes, "", &mut paths);
    paths
}

fn flatten_inner(nodes: &[MetaTreeNode], prefix: &str, paths: &mut Vec<String>) {
    for node in nodes {
        let full_path = if prefix.is_empty() {
            node.info.path.clone()
        } else {
            format!("{}/{}", prefix, node.info.path)
        };
        paths.push(full_path.clone());
        flatten_inner(&node.children, &full_path, paths);
    }
}

fn walk_inner(
    base_dir: &Path,
    projects: &[ProjectInfo],
    max_depth: usize,
    current_depth: usize,
    visited: &mut std::collections::HashSet<PathBuf>,
) -> Vec<MetaTreeNode> {
    let mut nodes = Vec::new();

    for project in projects {
        let project_dir = base_dir.join(&project.path);

        // Check if this project has its own .meta file directly in its directory
        let has_meta = project_dir.is_dir()
            && find_meta_config(&project_dir, None)
                .map(|(path, _)| {
                    path.parent()
                        .map(|p| p == project_dir)
                        .unwrap_or(false)
                })
                .unwrap_or(false);

        // Recurse into children if within depth limit and this is a meta repo
        let children = if has_meta && current_depth < max_depth {
            let canonical = project_dir.canonicalize().unwrap_or(project_dir.clone());
            if visited.insert(canonical) {
                if let Some((nested_config_path, _)) = find_meta_config(&project_dir, None) {
                    if let Ok((nested_projects, _)) = parse_meta_config(&nested_config_path) {
                        walk_inner(
                            &project_dir,
                            &nested_projects,
                            max_depth,
                            current_depth + 1,
                            visited,
                        )
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            } else {
                vec![] // Cycle detected
            }
        } else {
            vec![]
        };

        nodes.push(MetaTreeNode {
            info: project.clone(),
            is_meta: has_meta,
            children,
        });
    }

    nodes.sort_by(|a, b| a.info.name.cmp(&b.info.name));
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_walk_meta_tree_no_config() {
        let dir = tempfile::tempdir().unwrap();
        let result = walk_meta_tree(dir.path(), None);
        assert!(result.is_err());
    }

    #[test]
    fn test_walk_meta_tree_empty_projects() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".meta"), r#"{"projects": {}}"#).unwrap();
        let tree = walk_meta_tree(dir.path(), None).unwrap();
        assert!(tree.is_empty());
    }

    #[test]
    fn test_walk_meta_tree_multiple_projects_sorted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("zebra")).unwrap();
        std::fs::create_dir(dir.path().join("alpha")).unwrap();
        std::fs::create_dir(dir.path().join("middle")).unwrap();
        std::fs::write(
            dir.path().join(".meta"),
            r#"{"projects": {
                "zebra": "git@github.com:org/zebra.git",
                "alpha": "git@github.com:org/alpha.git",
                "middle": "git@github.com:org/middle.git"
            }}"#,
        )
        .unwrap();

        let tree = walk_meta_tree(dir.path(), None).unwrap();
        assert_eq!(tree.len(), 3);
        assert_eq!(tree[0].info.name, "alpha");
        assert_eq!(tree[1].info.name, "middle");
        assert_eq!(tree[2].info.name, "zebra");
    }

    #[test]
    fn test_walk_meta_tree_is_meta_flag() {
        let dir = tempfile::tempdir().unwrap();
        let has_meta = dir.path().join("has_meta");
        let no_meta = dir.path().join("no_meta");
        std::fs::create_dir(&has_meta).unwrap();
        std::fs::create_dir(&no_meta).unwrap();
        std::fs::write(has_meta.join(".meta"), r#"{"projects": {}}"#).unwrap();
        std::fs::write(
            dir.path().join(".meta"),
            r#"{"projects": {
                "has_meta": "git@github.com:org/has_meta.git",
                "no_meta": "git@github.com:org/no_meta.git"
            }}"#,
        )
        .unwrap();

        let tree = walk_meta_tree(dir.path(), None).unwrap();
        let has = tree.iter().find(|n| n.info.name == "has_meta").unwrap();
        let no = tree.iter().find(|n| n.info.name == "no_meta").unwrap();
        assert!(has.is_meta);
        assert!(!no.is_meta);
    }

    #[test]
    fn test_walk_meta_tree_depth_zero_no_recursion() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        let grandchild = child.join("grandchild");
        std::fs::create_dir_all(&grandchild).unwrap();
        std::fs::write(
            dir.path().join(".meta"),
            r#"{"projects": {"child": "git@github.com:org/child.git"}}"#,
        )
        .unwrap();
        std::fs::write(
            child.join(".meta"),
            r#"{"projects": {"grandchild": "git@github.com:org/grandchild.git"}}"#,
        )
        .unwrap();

        let tree = walk_meta_tree(dir.path(), Some(0)).unwrap();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].info.name, "child");
        assert!(tree[0].is_meta);
        assert!(tree[0].children.is_empty()); // No recursion
    }

    #[test]
    fn test_walk_meta_tree_cycle_detection() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        std::fs::create_dir(&child).unwrap();

        // Create a symlink from child/loop back to root
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path(), child.join("loop")).unwrap();
        }

        std::fs::write(
            dir.path().join(".meta"),
            r#"{"projects": {"child": "git@github.com:org/child.git"}}"#,
        )
        .unwrap();
        std::fs::write(
            child.join(".meta"),
            r#"{"projects": {"loop": "git@github.com:org/loop.git"}}"#,
        )
        .unwrap();

        // Should not infinite loop - cycle detection stops recursion
        let tree = walk_meta_tree(dir.path(), None).unwrap();
        let paths = flatten_meta_tree(&tree);
        assert!(paths.contains(&"child".to_string()));
        // The cycle node is included but has no children
        assert!(paths.contains(&"child/loop".to_string()));
        let child_node = &tree[0];
        assert!(child_node.children[0].children.is_empty());
    }

    #[test]
    fn test_flatten_meta_tree_empty() {
        let paths = flatten_meta_tree(&[]);
        assert!(paths.is_empty());
    }

    #[test]
    fn test_flatten_meta_tree_deeply_nested() {
        let dir = tempfile::tempdir().unwrap();
        let l1 = dir.path().join("l1");
        let l2 = l1.join("l2");
        let l3 = l2.join("l3");
        std::fs::create_dir_all(&l3).unwrap();

        std::fs::write(
            dir.path().join(".meta"),
            r#"{"projects": {"l1": "git@github.com:org/l1.git"}}"#,
        )
        .unwrap();
        std::fs::write(
            l1.join(".meta"),
            r#"{"projects": {"l2": "git@github.com:org/l2.git"}}"#,
        )
        .unwrap();
        std::fs::write(
            l2.join(".meta"),
            r#"{"projects": {"l3": "git@github.com:org/l3.git"}}"#,
        )
        .unwrap();

        let tree = walk_meta_tree(dir.path(), None).unwrap();
        let paths = flatten_meta_tree(&tree);

        assert_eq!(paths.len(), 3);
        assert_eq!(paths[0], "l1");
        assert_eq!(paths[1], "l1/l2");
        assert_eq!(paths[2], "l1/l2/l3");
    }

    #[test]
    fn test_walk_meta_tree_nonexistent_project_dir() {
        let dir = tempfile::tempdir().unwrap();
        // Project listed in .meta but directory doesn't exist
        std::fs::write(
            dir.path().join(".meta"),
            r#"{"projects": {"missing": "git@github.com:org/missing.git"}}"#,
        )
        .unwrap();

        let tree = walk_meta_tree(dir.path(), None).unwrap();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].info.name, "missing");
        assert!(!tree[0].is_meta);
        assert!(tree[0].children.is_empty());
    }

    #[test]
    fn test_walk_meta_tree_extended_format() {
        let dir = tempfile::tempdir().unwrap();
        let custom_path = dir.path().join("custom/path");
        std::fs::create_dir_all(&custom_path).unwrap();
        std::fs::write(
            dir.path().join(".meta"),
            r#"{"projects": {
                "myproject": {
                    "repo": "git@github.com:org/myproject.git",
                    "path": "custom/path",
                    "tags": ["frontend", "react"]
                }
            }}"#,
        )
        .unwrap();

        let tree = walk_meta_tree(dir.path(), None).unwrap();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].info.name, "myproject");
        assert_eq!(tree[0].info.path, "custom/path");
        assert_eq!(tree[0].info.tags, vec!["frontend", "react"]);
    }
}
