//! Integration tests for ConfigDiscovery module.

use echo_agent_app_core::config_discovery::{
    ConfigCategory, ConfigDiscovery, ConfigScope,
};
use std::path::PathBuf;

#[test]
fn test_config_discovery_creation() {
    let discovery = ConfigDiscovery::new();
    // Should not panic
    let inventory = discovery.discover_all();
    // Inventory should have some entries even if files don't exist
    assert!(inventory.total_count() > 0);
}

#[test]
fn test_config_discovery_with_explicit_paths() {
    let tmp = std::env::temp_dir().join("echo-config-integration-test");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join(".echo-agent")).unwrap();

    // Create a test config file
    std::fs::write(
        tmp.join(".echo-agent").join("user.md"),
        "# User Instructions\nBe helpful.",
    )
    .unwrap();

    let discovery = ConfigDiscovery::with_paths(
        tmp.clone(),
        tmp.clone(),
        Some(tmp.clone()),
    );

    let inventory = discovery.discover_all();
    assert!(inventory.total_count() > 0);

    // Check that user.md is found
    let user_instructions: Vec<_> = inventory
        .instructions
        .iter()
        .filter(|f| f.name == "user.md")
        .collect();
    assert!(!user_instructions.is_empty());
    assert!(user_instructions[0].accessible);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_config_inventory_filtering() {
    let tmp = std::env::temp_dir().join("echo-config-filter-test");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join(".echo-agent")).unwrap();

    let discovery = ConfigDiscovery::with_paths(
        tmp.clone(),
        tmp.clone(),
        Some(tmp.clone()),
    );

    let inventory = discovery.discover_all();

    // Filter by scope
    let global_files = inventory.by_scope(ConfigScope::Global);
    assert!(!global_files.is_empty());

    // Filter by category
    let instruction_files = inventory.by_category(ConfigCategory::Instructions);
    assert!(!instruction_files.is_empty());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_config_scope_display() {
    assert_eq!(ConfigScope::Global.to_string(), "global");
    assert_eq!(ConfigScope::Project.to_string(), "project");
    assert_eq!(ConfigScope::Local.to_string(), "local");
    assert_eq!(ConfigScope::Plugin.to_string(), "plugin");
}

#[test]
fn test_config_category_display() {
    assert_eq!(ConfigCategory::Agent.to_string(), "agent");
    assert_eq!(ConfigCategory::Mcp.to_string(), "mcp");
    assert_eq!(ConfigCategory::Hooks.to_string(), "hooks");
    assert_eq!(ConfigCategory::Instructions.to_string(), "instructions");
    assert_eq!(ConfigCategory::Plugin.to_string(), "plugin");
    assert_eq!(ConfigCategory::Workspace.to_string(), "workspace");
    assert_eq!(ConfigCategory::Lsp.to_string(), "lsp");
}
