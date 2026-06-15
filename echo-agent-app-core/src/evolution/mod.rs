//! Product-layer evolution integration.
//!
//! Bridges the framework's [`MemoryReviewer`] into the application lifecycle:
//! automatic review on session end, review every N writes, and manual
//! `/memory-review` command.

pub mod dashboard;
pub mod review_integration;
pub mod rule_promoter;

pub use dashboard::{ActivityEntry, Dashboard, DashboardMetrics, MemoryStats, SkillHealthOverview};
pub use review_integration::{ReviewIntegration, discover_echo_agent_dir, format_review_report};
pub use rule_promoter::{PromotionCriteria, RulePromoter, RuleProposal};
