//! Skills Hub — 本地技能市场
//!
//! 扫描 `~/.echo-agent/skills/` 目录，提供搜索、安装、查看详情功能。

pub mod install;
pub mod registry;

pub use registry::{SkillHubEntry, SkillsHub};
