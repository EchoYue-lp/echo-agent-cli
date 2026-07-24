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
use echo_agent::config::AppConfig;
use echo_agent::state::RuntimeStateStore;
use echo_agent_app_core::infra::{self, AgentCreateParams};
use echo_agent_app_core::runtime_state_file::FileRuntimeStateStore;

fn make_app_config() -> AppConfig {
    let mut c = AppConfig::default();
    // Pin a known model name and small token limit so create_agent doesn't
    // try to load an external model registry.
    c.agent.name = "rt-test-agent".to_string();
    c.agent.system_prompt = "You are a test runtime agent.".to_string();
    c.agent.max_iterations = 1;
    c.agent.token_limit = 4096;
    c.model.provider = "ollama".to_string(); // no auth_token needed
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
async fn create_agent_threads_state_store_and_conversation_id() {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn RuntimeStateStore> =
        Arc::new(FileRuntimeStateStore::new(tmp.path()).unwrap());

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
    };
    let app_config = make_app_config();
    let agent = infra::create_agent(&params, &app_config)
        .await
        .expect("create_agent should succeed in e2e test — check model/provider config");

    assert!(
        agent.state_store().is_some(),
        "state_store must be threaded through create_agent into ReactAgent"
    );
    assert_eq!(
        agent.conversation_id(),
        Some("conv-xyz"),
        "conversation_id must be threaded through create_agent into AgentConfig"
    );
}

#[tokio::test]
async fn create_agent_without_state_store_leaves_it_none() {
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
    };
    let app_config = make_app_config();
    let agent = infra::create_agent(&params, &app_config)
        .await
        .expect("create_agent should succeed in e2e test — check model/provider config");

    assert!(
        agent.state_store().is_none(),
        "state_store stays None when caller doesn't supply one"
    );
    assert!(
        agent.conversation_id().is_none(),
        "conversation_id stays None when caller doesn't supply one"
    );
}

#[tokio::test]
async fn memory_context_suffix_lands_in_system_prompt() {
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
    };
    let app_config = make_app_config();
    let agent = infra::create_agent(&params, &app_config)
        .await
        .expect("create_agent should succeed in e2e test — check model/provider config");

    let prompt = agent.system_prompt();
    assert!(
        prompt.contains(unique),
        "system prompt must contain the memory_context_suffix marker. \
         prompt was: {prompt}"
    );
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
