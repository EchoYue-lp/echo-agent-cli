//! Stable app-core facade for CLI, TUI, Tauri, channel and integration users.
//!
//! These modules are intentionally thin re-export views. The implementation
//! modules remain the sole authority for state, persistence, execution and
//! publication; this facade only makes the supported boundary explicit.
//!
//! Direct implementation-module imports are intentionally rejected outside
//! the crate. The facade is the supported public boundary:
//!
//! ```compile_fail
//! use echo_agent_app_core::state::AppState;
//! ```
//!
//! ```compile_fail
//! use echo_agent_app_core::AppState;
//! ```
//!
//! ```
//! use echo_agent_app_core::api::AppState;
//! ```

pub mod agent_control {
    pub use crate::agent_control::*;
}
pub mod agent_handle {
    pub use crate::agent_handle::*;
}
pub mod agent_pool {
    pub use crate::agent_pool::*;
}
pub mod agent_router {
    pub use crate::agent_router::*;
}
pub mod analysis {
    pub use crate::analysis::*;
}
pub mod analysis_runtime {
    pub use crate::analysis_runtime::*;
}
pub mod attachments {
    pub use crate::attachments::*;
}
pub mod auto_memory {
    pub use crate::auto_memory::*;
}
pub mod browser {
    pub use crate::browser::*;
}
pub mod chat_driver {
    pub use crate::chat_driver::*;
}
pub mod chat_event_log {
    pub use crate::chat_event_log::*;
}
pub mod chat_resources {
    pub use crate::chat_resources::*;
}
pub mod config {
    pub use crate::config::*;
}
pub mod config_discovery {
    pub use crate::config_discovery::*;
}
pub mod config_watcher {
    pub use crate::config_watcher::*;
}
pub mod context_window {
    pub use crate::context_window::*;
}
pub mod conversation_deletion {
    pub use crate::conversation_deletion::*;
}
pub mod conversation_input {
    pub use crate::conversation_input::*;
}
pub mod conversation_projection {
    pub use crate::conversation_projection::*;
}
pub mod data_root {
    pub use crate::data_root::*;
}
pub mod developer_commands {
    pub use crate::developer_commands::*;
}
pub mod diff {
    pub use crate::diff::*;
}
pub mod error {
    pub use crate::error::*;
}
pub mod export {
    pub use crate::export::*;
}
pub mod evolution {
    pub use crate::evolution::*;
}
pub mod extension_commands {
    pub use crate::extension_commands::*;
}
pub mod extension_control {
    pub use crate::extension_control::*;
}
pub mod foreground_turn {
    pub use crate::foreground_turn::*;
}
pub mod hitl {
    pub use crate::hitl::*;
}
pub mod hook_config_loader {
    pub use crate::hook_config_loader::*;
}
pub mod infra {
    pub use crate::infra::*;
}
pub mod instruction_provider {
    pub use crate::instruction_provider::*;
}
pub mod manual_compression {
    pub use crate::manual_compression::*;
}
pub mod mcp_config_runtime {
    pub use crate::mcp_config_runtime::*;
}
pub mod model_config {
    pub use crate::model_config::*;
}
pub mod observability {
    pub use crate::observability::*;
}
pub mod output {
    pub use crate::output::*;
}
pub mod permission {
    pub use crate::permission::*;
}
pub mod plugin_runtime {
    pub use crate::plugin_runtime::*;
}
pub mod prepared_turn {
    pub use crate::prepared_turn::*;
}
pub mod product_data_io {
    pub use crate::product_data_io::*;
}
pub mod profiles {
    pub use crate::profiles::*;
}
pub mod project {
    pub use crate::project::*;
}
pub mod prompt_contract {
    pub use crate::prompt_contract::*;
}
pub mod reflection {
    pub use crate::reflection::*;
}
pub mod research {
    pub use crate::research::*;
}
pub mod research_connectors {
    pub use crate::research_connectors::*;
}
pub mod research_tool {
    pub use crate::research_tool::*;
}
pub mod run_driver {
    pub use crate::run_driver::*;
}
pub mod runtime {
    pub use crate::runtime::*;
}
pub mod scheduler {
    pub use crate::scheduler::*;
}
pub mod skills_hub {
    pub use crate::skills_hub::*;
}
pub mod state {
    pub use crate::state::*;
}
pub mod structured_extraction {
    pub use crate::structured_extraction::*;
}
pub mod subagent_loader {
    pub use crate::subagent_loader::*;
}
pub mod subagent_prompt {
    pub use crate::subagent_prompt::*;
}
pub mod tasks {
    pub use crate::tasks::*;
}
pub mod terminal {
    pub use crate::terminal::*;
}
pub mod tool_control {
    pub use crate::tool_control::*;
}
pub mod tool_execution {
    pub use crate::tool_execution::*;
}
pub mod tool_execution_projection {
    pub use crate::tool_execution_projection::*;
}
pub mod turn_context {
    pub use crate::turn_context::*;
}
pub mod types {
    pub use crate::types::*;
}
pub mod unified_memory {
    pub use crate::unified_memory::*;
}
pub mod utils {
    pub use crate::utils::*;
}
pub mod webhook {
    pub use crate::webhook::*;
}
pub mod workflow_service {
    pub use crate::workflow_service::*;
}
pub mod workspace {
    pub use crate::workspace::*;
}
pub mod workspace_routing {
    pub use crate::workspace_routing::*;
}

pub use crate::state::AppState;
