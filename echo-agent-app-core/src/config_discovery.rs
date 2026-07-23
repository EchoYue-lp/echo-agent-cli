//! Unified configuration discovery — find all config files in one place.
//!
//! EchoAgent uses multiple configuration files scattered across different
//! locations. This module provides a single entry point to discover all of them.
//!
//! ## Config file inventory
//!
//! 全局用户目录根为 `~/.eko/`(由 `echo_agent::paths` 统一解析,应用启动时设置)。
//!
//! | File | Location | Purpose |
//! |------|----------|---------|
//! | `echo-agent.yaml` | Project root or `~/.eko/` | Agent configuration |
//! | `.mcp.json` | Project root or `~/.eko/` | MCP server configuration |
//! | `hooks.yaml` | Project `.eko/` or `~/.eko/` | Hook definitions |
//! | `user.md` | `~/.eko/` | User-level instructions |
//! | `project.md` | `<project-root>/.eko/` | Project-level instructions |
//! | `local.md` | `<cwd>/.eko/` | Local directory instructions |
//! | `manifest.yaml` | Plugin directories | Plugin manifests |
//! | `.workspace.json` | Workspace directories | Workspace metadata |
//! | `.lsp.yaml` | Project root | LSP server configuration |

use std::path::PathBuf;

/// A discovered configuration file.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    /// Human-readable name.
    pub name: String,
    /// Absolute path to the file.
    pub path: PathBuf,
    /// What scope this config belongs to.
    pub scope: ConfigScope,
    /// Category of the config file.
    pub category: ConfigCategory,
    /// Whether the file exists and is readable.
    pub accessible: bool,
}

/// Scope of a configuration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigScope {
    /// Global user configuration (`~/.eko/`).
    Global,
    /// Project-level configuration (`.eko/` in project root).
    Project,
    /// Local directory configuration (`.eko/` in cwd).
    Local,
    /// Plugin-specific configuration.
    Plugin,
}

impl std::fmt::Display for ConfigScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Global => write!(f, "global"),
            Self::Project => write!(f, "project"),
            Self::Local => write!(f, "local"),
            Self::Plugin => write!(f, "plugin"),
        }
    }
}

/// Category of a configuration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigCategory {
    /// Agent behavior configuration.
    Agent,
    /// MCP server connections.
    Mcp,
    /// Hook definitions.
    Hooks,
    /// Project rules / instructions.
    Instructions,
    /// Plugin manifests.
    Plugin,
    /// Workspace metadata.
    Workspace,
    /// LSP server configuration.
    Lsp,
}

impl std::fmt::Display for ConfigCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Agent => write!(f, "agent"),
            Self::Mcp => write!(f, "mcp"),
            Self::Hooks => write!(f, "hooks"),
            Self::Instructions => write!(f, "instructions"),
            Self::Plugin => write!(f, "plugin"),
            Self::Workspace => write!(f, "workspace"),
            Self::Lsp => write!(f, "lsp"),
        }
    }
}

/// Complete inventory of all discovered configuration files.
#[derive(Debug, Clone, Default)]
pub struct ConfigInventory {
    /// Agent configuration files.
    pub agent_configs: Vec<ConfigFile>,
    /// MCP configuration files.
    pub mcp_configs: Vec<ConfigFile>,
    /// Hook configuration files.
    pub hooks_configs: Vec<ConfigFile>,
    /// Instruction files (user.md, project.md, local.md).
    pub instructions: Vec<ConfigFile>,
    /// Plugin manifest files.
    pub plugin_manifests: Vec<ConfigFile>,
    /// Workspace metadata files.
    pub workspace_configs: Vec<ConfigFile>,
    /// LSP configuration files.
    pub lsp_configs: Vec<ConfigFile>,
}

impl ConfigInventory {
    /// Total number of discovered files.
    pub fn total_count(&self) -> usize {
        self.agent_configs.len()
            + self.mcp_configs.len()
            + self.hooks_configs.len()
            + self.instructions.len()
            + self.plugin_manifests.len()
            + self.workspace_configs.len()
            + self.lsp_configs.len()
    }

    /// Get all files across all categories.
    pub fn all_files(&self) -> Vec<&ConfigFile> {
        let mut all = Vec::new();
        all.extend(self.agent_configs.iter());
        all.extend(self.mcp_configs.iter());
        all.extend(self.hooks_configs.iter());
        all.extend(self.instructions.iter());
        all.extend(self.plugin_manifests.iter());
        all.extend(self.workspace_configs.iter());
        all.extend(self.lsp_configs.iter());
        all
    }

    /// Get files filtered by scope.
    pub fn by_scope(&self, scope: ConfigScope) -> Vec<&ConfigFile> {
        self.all_files()
            .into_iter()
            .filter(|f| f.scope == scope)
            .collect()
    }

    /// Get files filtered by category.
    pub fn by_category(&self, category: ConfigCategory) -> Vec<&ConfigFile> {
        self.all_files()
            .into_iter()
            .filter(|f| f.category == category)
            .collect()
    }
}

/// Configuration file discovery service.
pub struct ConfigDiscovery {
    /// Project root directory (if detected).
    project_root: Option<PathBuf>,
    /// Global user-data directory (`~/.eko/`, from `echo_agent::paths`).
    data_root: PathBuf,
    /// Current working directory.
    cwd: PathBuf,
}

impl ConfigDiscovery {
    /// Create a new discovery service.
    pub fn new() -> Self {
        let data_root = echo_agent::paths::user_data_dir();

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let project_root = crate::utils::find_project_root(&cwd);

        Self {
            project_root,
            data_root,
            cwd,
        }
    }

    /// Create with explicit paths (for testing). The global scope is placed
    /// under `home_dir/.eko` to mirror the real [`Self::new`] layout.
    pub fn with_paths(home_dir: PathBuf, cwd: PathBuf, project_root: Option<PathBuf>) -> Self {
        Self {
            project_root,
            data_root: home_dir.join(".eko"),
            cwd,
        }
    }

    /// Discover all configuration files.
    pub fn discover_all(&self) -> ConfigInventory {
        let mut inventory = ConfigInventory::default();

        self.discover_agent_configs(&mut inventory);
        self.discover_mcp_configs(&mut inventory);
        self.discover_hooks_configs(&mut inventory);
        self.discover_instructions(&mut inventory);
        self.discover_plugin_manifests(&mut inventory);
        self.discover_workspace_configs(&mut inventory);
        self.discover_lsp_configs(&mut inventory);

        inventory
    }

    /// List all discovered files as a flat vector.
    pub fn list_files(&self) -> Vec<ConfigFile> {
        self.discover_all()
            .all_files()
            .into_iter()
            .cloned()
            .collect()
    }

    // ── Discovery methods ────────────────────────────────────────────

    fn discover_agent_configs(&self, inv: &mut ConfigInventory) {
        // Global: ~/.eko/echo-agent.yaml
        let global = self.data_root.join("echo-agent.yaml");
        inv.agent_configs.push(ConfigFile {
            name: "echo-agent.yaml (global)".into(),
            path: global.clone(),
            scope: ConfigScope::Global,
            category: ConfigCategory::Agent,
            accessible: global.exists(),
        });

        // Project: <root>/echo-agent.yaml
        if let Some(ref root) = self.project_root {
            let project = root.join("echo-agent.yaml");
            inv.agent_configs.push(ConfigFile {
                name: "echo-agent.yaml (project)".into(),
                path: project.clone(),
                scope: ConfigScope::Project,
                category: ConfigCategory::Agent,
                accessible: project.exists(),
            });
        }
    }

    fn discover_mcp_configs(&self, inv: &mut ConfigInventory) {
        // Global: ~/.eko/mcp.json
        let global = self.data_root.join("mcp.json");
        inv.mcp_configs.push(ConfigFile {
            name: "mcp.json (global)".into(),
            path: global.clone(),
            scope: ConfigScope::Global,
            category: ConfigCategory::Mcp,
            accessible: global.exists(),
        });

        // Project: <root>/.mcp.json
        if let Some(ref root) = self.project_root {
            let project = root.join(".mcp.json");
            inv.mcp_configs.push(ConfigFile {
                name: ".mcp.json (project)".into(),
                path: project.clone(),
                scope: ConfigScope::Project,
                category: ConfigCategory::Mcp,
                accessible: project.exists(),
            });
        }
    }

    fn discover_hooks_configs(&self, inv: &mut ConfigInventory) {
        // Global: ~/.eko/hooks.yaml
        let global = self.data_root.join("hooks.yaml");
        inv.hooks_configs.push(ConfigFile {
            name: "hooks.yaml (global)".into(),
            path: global.clone(),
            scope: ConfigScope::Global,
            category: ConfigCategory::Hooks,
            accessible: global.exists(),
        });

        // Project: <root>/.eko/hooks.yaml
        if let Some(ref root) = self.project_root {
            let project = root.join(".eko").join("hooks.yaml");
            inv.hooks_configs.push(ConfigFile {
                name: "hooks.yaml (project)".into(),
                path: project.clone(),
                scope: ConfigScope::Project,
                category: ConfigCategory::Hooks,
                accessible: project.exists(),
            });
        }
    }

    fn discover_instructions(&self, inv: &mut ConfigInventory) {
        // User-level: ~/.eko/user.md
        let user = self.data_root.join("user.md");
        inv.instructions.push(ConfigFile {
            name: "user.md".into(),
            path: user.clone(),
            scope: ConfigScope::Global,
            category: ConfigCategory::Instructions,
            accessible: user.exists(),
        });

        // Project-level: <root>/.eko/project.md
        if let Some(ref root) = self.project_root {
            let project = root.join(".eko").join("project.md");
            inv.instructions.push(ConfigFile {
                name: "project.md".into(),
                path: project.clone(),
                scope: ConfigScope::Project,
                category: ConfigCategory::Instructions,
                accessible: project.exists(),
            });
        }

        // Local: <cwd>/.eko/local.md
        let local = self.cwd.join(".eko").join("local.md");
        inv.instructions.push(ConfigFile {
            name: "local.md".into(),
            path: local.clone(),
            scope: ConfigScope::Local,
            category: ConfigCategory::Instructions,
            accessible: local.exists(),
        });
    }

    fn discover_plugin_manifests(&self, inv: &mut ConfigInventory) {
        // Scan plugin directories in all scopes
        let scopes = [
            (ConfigScope::Global, self.data_root.join("plugins")),
            (
                ConfigScope::Project,
                self.project_root
                    .as_ref()
                    .map(|r| r.join(".eko").join("plugins"))
                    .unwrap_or_default(),
            ),
        ];

        for (scope, dir) in &scopes {
            if !dir.exists() {
                continue;
            }
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let manifest = entry.path().join(".echo-plugin").join("manifest.yaml");
                    if manifest.exists() {
                        inv.plugin_manifests.push(ConfigFile {
                            name: format!(
                                "manifest.yaml ({})",
                                entry.file_name().to_string_lossy()
                            ),
                            path: manifest,
                            scope: *scope,
                            category: ConfigCategory::Plugin,
                            accessible: true,
                        });
                    }
                }
            }
        }
    }

    fn discover_workspace_configs(&self, inv: &mut ConfigInventory) {
        let ws_dir = self.data_root.join("workspaces");
        if !ws_dir.exists() {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(&ws_dir) {
            for entry in entries.flatten() {
                let ws_file = entry.path().join(".workspace.json");
                if ws_file.exists() {
                    inv.workspace_configs.push(ConfigFile {
                        name: format!(".workspace.json ({})", entry.file_name().to_string_lossy()),
                        path: ws_file,
                        scope: ConfigScope::Global,
                        category: ConfigCategory::Workspace,
                        accessible: true,
                    });
                }
            }
        }
    }

    fn discover_lsp_configs(&self, inv: &mut ConfigInventory) {
        // Project: <root>/.lsp.yaml
        if let Some(ref root) = self.project_root {
            let project = root.join(".lsp.yaml");
            inv.lsp_configs.push(ConfigFile {
                name: ".lsp.yaml (project)".into(),
                path: project.clone(),
                scope: ConfigScope::Project,
                category: ConfigCategory::Lsp,
                accessible: project.exists(),
            });
        }

        // Global: ~/.eko/.lsp.yaml
        let global = self.data_root.join(".lsp.yaml");
        inv.lsp_configs.push(ConfigFile {
            name: ".lsp.yaml (global)".into(),
            path: global.clone(),
            scope: ConfigScope::Global,
            category: ConfigCategory::Lsp,
            accessible: global.exists(),
        });
    }
}

impl Default for ConfigDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_discovery_creation() {
        let discovery = ConfigDiscovery::new();
        // Should not panic
        let _inventory = discovery.discover_all();
    }

    #[test]
    fn test_config_inventory_empty() {
        let inv = ConfigInventory::default();
        assert_eq!(inv.total_count(), 0);
        assert!(inv.all_files().is_empty());
    }

    #[test]
    fn test_config_scope_display() {
        assert_eq!(ConfigScope::Global.to_string(), "global");
        assert_eq!(ConfigScope::Project.to_string(), "project");
        assert_eq!(ConfigScope::Local.to_string(), "local");
    }

    #[test]
    fn test_config_category_display() {
        assert_eq!(ConfigCategory::Agent.to_string(), "agent");
        assert_eq!(ConfigCategory::Mcp.to_string(), "mcp");
        assert_eq!(ConfigCategory::Hooks.to_string(), "hooks");
    }

    #[test]
    fn test_with_explicit_paths() {
        let tmp = std::env::temp_dir().join("echo-config-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let discovery = ConfigDiscovery::with_paths(tmp.clone(), tmp.clone(), Some(tmp.clone()));

        let inv = discovery.discover_all();
        // Should have entries (even if not accessible)
        assert!(inv.total_count() > 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
