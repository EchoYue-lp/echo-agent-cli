//! End-to-end checks for the product-layer runtime state wiring.
//!
//! These tests guard against regressions of the review findings:
//!
//! 1. `infra::create_runtime_state_store_in` actually creates a (file-backed)
//!    runtime_state directory.
//! 2. `AgentCreateParams::state_store + conversation_id` flow through the
//!    builder so `agent.state_store()` is `Some` and `agent.conversation_id()`
//!    matches — without this both `save_runtime_checkpoint` and
//!    `save_transcript_projection` silently no-op.
//! 3. `AgentCreateParams::memory_context_suffix` is appended to the
//!    assembled system prompt — so user/project/local instructions
//!    actually reach the LLM.

use std::sync::Arc;

use echo_agent::agent::Agent;
use echo_agent::state::{FileRuntimeStateStore, RuntimeStateStore};
use echo_agent_app_core::api::config::{ConfiguredModel, EkoConfig, ModelProviderConfig};
use echo_agent_app_core::api::infra::{self, AgentCreateParams};

fn make_app_config() -> EkoConfig {
    let mut c = EkoConfig::default();
    c.agent.name = "rt-test-agent".to_string();
    c.agent.system_prompt = "You are a test runtime agent.".to_string();
    c.agent.max_iterations = 1;
    c.agent.token_limit = 4096;
    c.model_providers.insert(
        "test-provider".to_string(),
        ModelProviderConfig {
            name: "Test Provider".to_string(),
            base_url: Some("http://127.0.0.1:11434/v1".to_string()),
            requires_api_key: false,
            ..ModelProviderConfig::default()
        },
    );
    c.configured_models.push(ConfiguredModel {
        id: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        provider: "test-provider".to_string(),
        model: "test-model".to_string(),
        ..ConfiguredModel::default()
    });
    c
}

#[tokio::test]
async fn create_runtime_state_store_creates_file_dir() -> Result<(), String> {
    // Use the explicit base-dir entrypoint. Mutating HOME is process-global and
    // races other integration tests that initialize the cached user data path.
    let tmp = tempfile::tempdir().map_err(|error| error.to_string())?;
    let base_dir = tmp.path().join(".echo-agent");
    let _store = infra::create_runtime_state_store_in(&base_dir)
        .ok_or_else(|| "create_runtime_state_store_in should succeed".to_string())?;

    let dir = base_dir.join("runtime_state");
    if !dir.is_dir() {
        return Err("runtime_state/ dir must be created under the requested base path".to_string());
    }
    Ok(())
}

#[tokio::test]
async fn create_agent_threads_state_store_and_conversation_id() -> Result<(), String> {
    let tmp = tempfile::tempdir().map_err(|error| format!("create temp directory: {error}"))?;
    let store: Arc<dyn RuntimeStateStore> = Arc::new(
        FileRuntimeStateStore::new(tmp.path())
            .map_err(|error| format!("create file runtime state store: {error}"))?,
    );

    let params = AgentCreateParams {
        model: Some("test-model".to_string()),
        system_prompt: Some("base".to_string()),
        project: None,
        session_id: Some("sess-1".to_string()),
        conversation_id: Some("conv-xyz".to_string()),
        react_checkpoint_interval: None,
        state_store: Some(store.clone()),
        memory_context_suffix: None,
        working_dir: None,
        task_runtime_store: None,
        browser_runtime: None,
        command_cell_runtime: None,
        product_data_io: Some(
            echo_agent_app_core::api::product_data_io::ProductDataIoService::new(),
        ),
        execution_scope: None,
    };
    let app_config = make_app_config();
    let agent = infra::create_agent(&params, &app_config)
        .await
        .map_err(|error| format!("create_agent should succeed in e2e test: {error}"))?;

    assert!(
        agent.state_store().is_some(),
        "state_store must be threaded through create_agent into ReactAgent"
    );
    assert_eq!(
        agent.conversation_id(),
        Some("conv-xyz"),
        "conversation_id must be threaded through create_agent into AgentConfig"
    );
    Ok(())
}

#[tokio::test]
async fn create_agent_without_explicit_state_store_uses_product_default() -> Result<(), String> {
    let params = AgentCreateParams {
        model: Some("test-model".to_string()),
        system_prompt: Some("base".to_string()),
        project: None,
        session_id: None,
        conversation_id: None,
        react_checkpoint_interval: None,
        state_store: None,
        memory_context_suffix: None,
        working_dir: None,
        task_runtime_store: None,
        browser_runtime: None,
        command_cell_runtime: None,
        product_data_io: Some(
            echo_agent_app_core::api::product_data_io::ProductDataIoService::new(),
        ),
        execution_scope: None,
    };
    let app_config = make_app_config();
    let agent = infra::create_agent(&params, &app_config)
        .await
        .map_err(|error| format!("create_agent should succeed in e2e test: {error}"))?;

    assert!(
        agent.state_store().is_some(),
        "EKO installs its durable default when the caller doesn't supply a state store"
    );
    assert!(
        agent.conversation_id().is_none(),
        "conversation_id stays None when caller doesn't supply one"
    );
    Ok(())
}

#[tokio::test]
async fn memory_context_suffix_lands_in_system_prompt() -> Result<(), String> {
    let unique = "ZZZ_TEST_MEMORY_CONTEXT_MARKER_QQQ";
    let suffix = format!("\n\n## Test memory\n{unique}\n");

    let params = AgentCreateParams {
        model: Some("test-model".to_string()),
        system_prompt: Some("base prompt".to_string()),
        project: None,
        session_id: None,
        conversation_id: None,
        react_checkpoint_interval: None,
        state_store: None,
        memory_context_suffix: Some(suffix.clone()),
        working_dir: None,
        task_runtime_store: None,
        browser_runtime: None,
        command_cell_runtime: None,
        product_data_io: Some(
            echo_agent_app_core::api::product_data_io::ProductDataIoService::new(),
        ),
        execution_scope: None,
    };
    let app_config = make_app_config();
    let agent = infra::create_agent(&params, &app_config)
        .await
        .map_err(|error| format!("create_agent should succeed in e2e test: {error}"))?;

    let prompt = agent.system_prompt();
    assert!(
        prompt.contains(unique),
        "system prompt must contain the memory_context_suffix marker. \
         prompt was: {prompt}"
    );
    Ok(())
}

#[tokio::test]
async fn create_agent_exposes_prompt_assembly_diagnostics() -> Result<(), String> {
    let params = AgentCreateParams {
        model: Some("test-model".to_string()),
        system_prompt: Some("base prompt".to_string()),
        project: None,
        session_id: None,
        conversation_id: None,
        react_checkpoint_interval: None,
        state_store: None,
        memory_context_suffix: Some("## Durable instructions\nKeep evidence.".to_string()),
        working_dir: None,
        task_runtime_store: None,
        browser_runtime: None,
        command_cell_runtime: None,
        product_data_io: Some(
            echo_agent_app_core::api::product_data_io::ProductDataIoService::new(),
        ),
        execution_scope: None,
    };
    let created = infra::create_agent_with_diagnostics(&params, &make_app_config()).await?;

    if created.prompt_assembly.estimated_tokens == 0 {
        return Err("prompt assembly reported zero tokens".into());
    }
    for required in ["base", "assistant", "runtime", "instruction_context"] {
        if !created
            .prompt_assembly
            .modules
            .iter()
            .any(|module| module.name == required && module.included)
        {
            return Err(format!(
                "prompt assembly missing included module: {required}"
            ));
        }
    }
    if created.agent.system_prompt() != created.prompt_assembly.prompt {
        return Err("agent prompt diverged from prompt assembly report".into());
    }
    let serialized = serde_json::to_value(&created.prompt_assembly)
        .map_err(|error| format!("serialize prompt assembly: {error}"))?;
    if serialized.get("prompt").is_some() {
        return Err("prompt assembly diagnostics exposed the full prompt".into());
    }
    Ok(())
}
