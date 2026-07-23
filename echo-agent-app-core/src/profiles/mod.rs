//! 配置档案管理模块
//!
//! 支持命名配置档案的 CRUD 操作，存储在 `~/.eko/profiles/`。
//!
//! # 使用示例
//!
//! ```rust,ignore
//! use profiles::ProfileManager;
//!
//! let manager = ProfileManager::new();
//! manager.activate("production")?;
//! ```

pub mod manager;
pub mod types;

pub use manager::ProfileManager;
pub use types::{Profile, ProfileSummary};
