//! User hooks configuration —— 向后兼容 shim。
//!
//! **历史**:本模块原本包含 `HooksLoadResult` 和 `load_hooks_files()`, 是唯一的文件 hook
//! 加载入口。但三个 hook 来源(echo-agent.yaml 内嵌 + ~/.eko/hooks.yaml + .eko/hooks.yaml)
//! 的加载逻辑分散在多处(hooks_config.rs、infra.rs、runtime.rs、hooks.rs),各自
//! `clear_user_hooks()` 导致互相覆盖(audit P0-1)。
//!
//! **现状**:统一加载逻辑已迁至 [`crate::hook_config_loader`] 的
//! `HookConfigLoader`,它把三源合并成单个 `HooksDefinition` 后一次性
//! register,消除覆盖 bug。本模块只保留为向后兼容的 re-export shim,
//! 让现存调用点(`runtime.rs`、`tui/events.rs`、`hooks.rs`)无感切换。
//!
//! **新代码不应直接用 `load_hooks_files()`** —— 它只加载文件、不含
//! 内嵌 hooks,会丢 echo-agent.yaml 的 `hooks:` 字段(就是 P0-1 修的
//! bug)。请改用 `HookConfigLoader::load_merged` 或
//! `load_merged_from_disk`。

pub use crate::hook_config_loader::{HookConfigLoader, HookConfigSource, HooksLoadResult};

/// 仅加载两个 hooks.yaml 文件的向后兼容入口。
///
/// **警告**:本函数不含 echo-agent.yaml 内嵌 hooks,会丢内嵌配置。
/// 仅用于不关心内嵌 hooks 的诊断场景。bootstrap 和 `/hooks reload`
/// 应改用 `HookConfigLoader::load_merged_from_disk()`。
pub fn load_hooks_files() -> HooksLoadResult {
    HookConfigLoader::load_hooks_files()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_hooks_files_no_error_when_missing() {
        // 不应 panic,即使文件都不存在。
        let result = load_hooks_files();
        // 只验证不 panic(文件存在性取决于运行环境)。
        let _ = result.loaded_from.is_empty();
    }

    #[test]
    fn test_shim_delegates_to_loader() {
        // shim 应与 loader 返回一致(同一调用)。
        let a = load_hooks_files();
        let b = HookConfigLoader::load_hooks_files();
        assert_eq!(a.loaded_from.len(), b.loaded_from.len());
    }
}
