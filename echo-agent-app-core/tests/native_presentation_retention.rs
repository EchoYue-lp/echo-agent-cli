//! 原长历史用例的独立 Cargo target；不与 App-core 库测试共享同一并发进程。

#[test]
fn native_presentation_keeps_original_order_and_public_artifacts_after_4096_prune_cold_reopen()
-> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    echo_agent_app_core::test_utils::native_presentation_retention(temp.path())
}
