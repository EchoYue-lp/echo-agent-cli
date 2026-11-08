//! 原长历史用例的独立 Cargo target；不与 App-core 库测试共享同一并发进程。

#[test]
fn original_usage_survives_more_than_4096_events_prune_and_cold_reopen() -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    echo_agent_app_core::test_utils::native_usage_retention(temp.path())
}
