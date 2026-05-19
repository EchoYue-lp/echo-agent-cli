//! 配置模块 — 从 echo-agent 库重新导出
//!
//! 所有配置类型和加载逻辑由 `echo_agent::config` 提供。

pub use echo_agent::config::{
    AppConfig,
    load_config, apply_env_overrides,
};
