//! Integration tests for the plugin API routes.
//!
//! These tests verify the request/response types and route structure
//! for the plugin management API.

use echo_agent_server::routes::plugins::{
    DependencyInfo, InstallRequest, PluginInfo, UninstallRequest,
};

#[test]
fn test_install_request_deserialization() {
    let json = r#"{"source": "/path/to/plugin", "scope": "user"}"#;
    let req: InstallRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.source, "/path/to/plugin");
    assert_eq!(req.scope, "user");
}

#[test]
fn test_install_request_default_scope() {
    let json = r#"{"source": "https://github.com/echo/plugin.git"}"#;
    let req: InstallRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.source, "https://github.com/echo/plugin.git");
    assert_eq!(req.scope, "user"); // default
}

#[test]
fn test_uninstall_request_deserialization() {
    let json = r#"{"name": "my-plugin", "keep_data": true}"#;
    let req: UninstallRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.name, "my-plugin");
    assert!(req.keep_data);
}

#[test]
fn test_uninstall_request_default_keep_data() {
    let json = r#"{"name": "my-plugin"}"#;
    let req: UninstallRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.name, "my-plugin");
    assert!(!req.keep_data); // default false
}

#[test]
fn test_plugin_info_serialization() {
    let info = PluginInfo {
        name: "test-plugin".to_string(),
        display_name: "Test Plugin".to_string(),
        version: "1.0.0".to_string(),
        description: "A test plugin".to_string(),
        author: Some("Test Author".to_string()),
        license: Some("MIT".to_string()),
        scope: "user".to_string(),
        enabled: true,
        path: "/home/user/.echo-agent/plugins/test-plugin".to_string(),
        capabilities: vec!["Skills".to_string(), "Hooks".to_string()],
        keywords: vec!["test".to_string()],
        dependencies: vec![DependencyInfo {
            name: "base-tools".to_string(),
            version: Some(">=1.0.0".to_string()),
        }],
        config_keys: vec!["api_endpoint".to_string()],
    };

    let json = serde_json::to_string(&info).unwrap();
    assert!(json.contains("test-plugin"));
    assert!(json.contains("1.0.0"));
    assert!(json.contains("Skills"));

    // Roundtrip
    let deserialized: PluginInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.name, "test-plugin");
    assert_eq!(deserialized.version, "1.0.0");
    assert_eq!(deserialized.capabilities.len(), 2);
    assert_eq!(deserialized.dependencies.len(), 1);
}

#[test]
fn test_dependency_info_serialization() {
    let dep = DependencyInfo {
        name: "base-tools".to_string(),
        version: Some(">=2.0.0".to_string()),
    };
    let json = serde_json::to_string(&dep).unwrap();
    assert!(json.contains("base-tools"));
    assert!(json.contains(">=2.0.0"));
}

#[test]
fn test_dependency_info_without_version() {
    let dep = DependencyInfo {
        name: "simple-dep".to_string(),
        version: None,
    };
    let json = serde_json::to_string(&dep).unwrap();
    assert!(json.contains("simple-dep"));
    // version should be null
    assert!(json.contains("null"));
}
