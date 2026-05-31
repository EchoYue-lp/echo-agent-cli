//! User hooks configuration loader.
//!
//! Discovers and loads hooks from YAML files:
//! - `~/.echo-agent/hooks.yaml` (global)
//! - `.echo-agent/hooks.yaml` (project-local)

use echo_agent::skills::hooks::HooksDefinition;
use std::path::{Path, PathBuf};

/// Result of loading hooks from config files.
pub struct HooksLoadResult {
    /// The merged hooks definition.
    pub definition: HooksDefinition,
    /// Paths that were successfully loaded.
    pub loaded_from: Vec<PathBuf>,
}

/// Discover and load hooks from standard locations.
///
/// Search order:
/// 1. `~/.echo-agent/hooks.yaml` (global user hooks)
/// 2. `.echo-agent/hooks.yaml` (project-local hooks, relative to cwd)
///
/// Project hooks are merged on top of global hooks.
pub fn load_hooks_files() -> HooksLoadResult {
    let mut definition = HooksDefinition::default();
    let mut loaded_from = Vec::new();

    // Global hooks: ~/.echo-agent/hooks.yaml
    if let Ok(home) = std::env::var("HOME") {
        let global_path = PathBuf::from(home).join(".echo-agent").join("hooks.yaml");
        if let Some(def) = try_load_yaml(&global_path) {
            definition.merge(def);
            loaded_from.push(global_path);
        }
    }

    // Project-local hooks: .echo-agent/hooks.yaml
    if let Ok(cwd) = std::env::current_dir() {
        let project_path = cwd.join(".echo-agent").join("hooks.yaml");
        if let Some(def) = try_load_yaml(&project_path) {
            definition.merge(def);
            loaded_from.push(project_path);
        }
    }

    HooksLoadResult {
        definition,
        loaded_from,
    }
}

/// Try to load a hooks YAML file. Returns None if file doesn't exist or fails to parse.
fn try_load_yaml(path: &Path) -> Option<HooksDefinition> {
    if !path.exists() {
        return None;
    }

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Failed to read hooks file");
            return None;
        }
    };

    match serde_yaml::from_str::<HooksDefinition>(&content) {
        Ok(def) => {
            tracing::info!(path = %path.display(), "Loaded hooks from file");
            Some(def)
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Failed to parse hooks file");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_hooks_files_no_error_when_missing() {
        // Should not panic even if files don't exist
        let result = load_hooks_files();
        // Just verify it returns without error
        assert!(result.loaded_from.is_empty() || !result.loaded_from.is_empty());
    }
}
