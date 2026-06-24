//! End-to-end checks for the product-layer runtime state wiring.
//!
//! These tests guard against regressions of the review findings:
//!
//! 1. `infra::create_runtime_state_store` actually creates a sqlite file.
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
use echo_agent::memory::{InMemoryStore, Store};
use echo_agent::state::{RuntimeStateStore, SqliteRuntimeStateStore};
use echo_agent_app_core::infra::{self, AgentCreateParams};
use echo_agent_app_core::unified_memory::UnifiedMemory;

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
async fn create_runtime_state_store_creates_sqlite_file_with_schema() {
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

    let db = tmp.path().join(".echo-agent/runtime_state.db");
    assert!(
        db.exists(),
        "runtime_state.db must be created at the canonical path"
    );

    // Schema: opening with a second store on the same path should not error
    // and the agent_checkpoints / task_nodes tables must exist (init_tables
    // is idempotent — if the file is half-written this would panic).
    let _store2 = SqliteRuntimeStateStore::new(&db).expect("re-open existing sqlite store");
}

#[tokio::test]
async fn create_agent_threads_state_store_and_conversation_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("rt.db");
    let store: Arc<dyn RuntimeStateStore> =
        Arc::new(SqliteRuntimeStateStore::new(&db_path).unwrap());

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
        .expect("create_agent should succeed in e2e test — check model/provider config");

    let prompt = agent.system_prompt();
    assert!(
        prompt.contains(unique),
        "system prompt must contain the memory_context_suffix marker. \
         prompt was: {prompt}"
    );
}

#[tokio::test]
async fn unified_memory_with_store_round_trips_through_recall() {
    // Verify Phase 3.3: UnifiedMemory.with_store actually wires the Store
    // through so `recall(...)` returns what `remember(...)` wrote to it.
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
    let unified = UnifiedMemory::load().with_store(store.clone());

    // Use a marker phrase that won't collide with anything the loader read
    // from real user.md/project.md files.
    let marker = "UNIFIED_MEMORY_E2E_PROBE";
    let _key = unified
        .remember(marker, 0.9)
        .await
        .expect("remember should write to the attached store");

    let hits = unified
        .recall(marker)
        .await
        .expect("recall should query the attached store");
    assert!(
        hits.iter().any(|h| h.content.contains(marker)),
        "expected to recall the value we just remembered, got: {hits:?}"
    );

    // And: a UnifiedMemory without with_store must error on remember/recall —
    // this protects against silently swallowing missing-store config.
    let bare = UnifiedMemory::load();
    assert!(
        bare.remember("x", 0.5).await.is_err(),
        "remember on a UnifiedMemory without a store must surface an error"
    );
}
