pub(crate) mod agent_control;
pub(crate) mod agent_handle;
pub(crate) mod agent_pool;
pub(crate) mod agent_router;
pub(crate) mod analysis;
pub(crate) mod analysis_runtime;
pub(crate) mod attachments;
pub(crate) mod auto_memory;
pub(crate) mod browser;
pub(crate) mod chat_driver;
pub(crate) mod chat_event_log;
pub(crate) mod chat_resources;
pub(crate) mod config;
pub(crate) mod config_discovery;
pub(crate) mod config_watcher;
pub(crate) mod context_window;
pub(crate) mod conversation_archive;
pub(crate) mod conversation_deletion;
pub(crate) mod conversation_input;
pub(crate) mod conversation_projection;
pub(crate) mod data_root;
pub(crate) mod developer_commands;
pub(crate) mod diff;
pub(crate) mod error;
pub(crate) mod evolution;
pub(crate) mod export;
pub(crate) mod extension_commands;
pub(crate) mod extension_control;
#[cfg(test)]
mod f6_contracts;
pub(crate) mod foreground_turn;
pub(crate) mod hitl;
pub(crate) mod hook_config_loader;
pub(crate) mod infra;
pub(crate) mod instruction_provider;
pub(crate) mod manual_compression;
pub(crate) mod mcp_config_runtime;
pub(crate) mod model_config;
pub(crate) mod observability;
pub(crate) mod output;
pub(crate) mod permission;
mod plugin_components;
pub(crate) mod plugin_runtime;
pub(crate) mod prepared_turn;
pub(crate) mod product_data_io;
pub(crate) mod profiles;
pub(crate) mod project;
pub(crate) mod prompt_contract;
pub(crate) mod reflection;
pub(crate) mod research;
pub(crate) mod research_connectors;
pub(crate) mod research_tool;
pub(crate) mod run_driver;
pub(crate) mod scheduler;
pub(crate) mod skills_hub;
pub(crate) mod state;
pub(crate) mod structured_extraction;
pub(crate) mod subagent_loader;
pub(crate) mod subagent_prompt;
pub(crate) mod tasks;
pub(crate) mod terminal;
pub(crate) mod tool_control;
pub(crate) mod tool_execution;
pub(crate) mod tool_execution_projection;
pub(crate) mod tool_exposure;
pub(crate) mod turn_context;
pub(crate) mod types;
pub(crate) mod unified_memory;
pub(crate) mod utils;
pub(crate) mod webhook;
pub(crate) mod workflow_service;
pub(crate) mod workspace;
pub(crate) mod workspace_routing;

pub(crate) mod runtime;

/// Stable application-facing facade.
///
/// Surface crates should import through this module. The child modules are
/// compatibility views over the single app-core implementations; they do not
/// own state, persistence, scheduling, or publication logic.
pub mod api;
