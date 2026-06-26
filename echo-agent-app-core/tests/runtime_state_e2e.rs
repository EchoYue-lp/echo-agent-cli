//! End-to-end checks for the product-layer runtime state wiring.
//!
//! These tests guard against regressions of the review findings:
//!
//! 1. `infra::create_runtime_state_store` actually creates a (file-backed)
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
async fn create_runtime_state_store_creates_file_dir() {
    // Override HOME so the helper writes into a tempdir and can't collide
    // with the developer's real ~/.echo-agent/.
    let tmp = tempfile::tempdir().unwrap();
    // SAFETY: integration tests run in their own process; setting HOME here
    // is constrained to this test binary's lifetime.
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }

    let store = infra::create_runtime_state_store();
    assert!(store.is_some(), "create_runtime_state_store should succeed");

    let dir = tmp.path().join(".echo-agent/runtime_state");
    assert!(
        dir.exists(),
        "runtime_state/ dir must be created at the canonical path"
    );
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
        route: None,
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
        route: None,
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
        route: None,
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
