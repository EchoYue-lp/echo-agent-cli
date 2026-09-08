//! Skills Hub — 本地技能市场
//!
//! 扫描 `~/.eko/skills/` 目录，提供搜索、安装、查看详情功能。

pub mod enabled_skills;
pub mod install;
pub(crate) mod project;
pub mod registry;

pub use enabled_skills::EnabledSkillsConfig;
pub(crate) use install::sync_skills;
pub use install::{SkillSourceRecord, SkillUpdateState, SkillUpdateStatus, check_updates};
pub(crate) use project::load_project_skills;
pub use registry::{SkillHubEntry, SkillsHub};
