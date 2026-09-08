//! Skills Hub — 本地技能市场
//!
//! 扫描 `~/.eko/skills/` 目录，提供搜索、安装、查看详情功能。

pub mod enabled_skills;
pub mod install;
pub mod registry;

pub use enabled_skills::EnabledSkillsConfig;
pub(crate) use install::sync_skills;
pub use install::{SkillSourceRecord, SkillUpdateState, SkillUpdateStatus, check_updates};
pub use registry::{SkillHubEntry, SkillsHub};
