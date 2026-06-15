//! Evolution dashboard — provides metrics and status overview.
//!
//! The dashboard aggregates evolution system metrics including:
//! - Memory statistics by type and status
//! - Skill health overview
//! - Recent evolution activities
//! - Promotion candidates

use chrono::{DateTime, Utc};
use echo_agent::evolution::{ChangeLog, JsonlChangeLog};
use echo_agent::memory::{MemoryFilter, MemoryStatus, MemoryType, Store, TypedMemoryStore};
use echo_agent::workspace::state::skill_telemetry::SkillTelemetryStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Dashboard metrics summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardMetrics {
    /// Memory statistics by type.
    pub memory_by_type: HashMap<MemoryType, MemoryStats>,
    /// Memory statistics by status.
    pub memory_by_status: HashMap<MemoryStatus, usize>,
    /// Total number of memories.
    pub total_memories: usize,
    /// Skill health overview.
    pub skill_health: SkillHealthOverview,
    /// Recent evolution activities (last 10).
    pub recent_activities: Vec<ActivityEntry>,
    /// Timestamp when metrics were generated.
    pub generated_at: DateTime<Utc>,
}

/// Statistics for a specific memory type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    /// Total count of memories of this type.
    pub count: usize,
    /// Average confidence score.
    pub avg_confidence: f32,
    /// Number of active memories.
    pub active_count: usize,
    /// Number of archived memories.
    pub archived_count: usize,
}

/// Skill health overview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillHealthOverview {
    /// Total number of skills with telemetry.
    pub total_skills: usize,
    /// Number of healthy skills (success rate > 0.7).
    pub healthy_skills: usize,
    /// Number of unhealthy skills (success rate < 0.5).
    pub unhealthy_skills: usize,
    /// Number of skills needing attention (success rate 0.5-0.7).
    pub needs_attention: usize,
    /// Average success rate across all skills.
    pub avg_success_rate: f32,
}

/// An evolution activity entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    /// Type of activity.
    pub activity_type: String,
    /// Description of the activity.
    pub description: String,
    /// When the activity occurred.
    pub timestamp: DateTime<Utc>,
}

/// Dashboard for evolution system metrics.
pub struct Dashboard {
    store: Arc<dyn Store>,
    change_log: JsonlChangeLog,
}

impl Dashboard {
    /// Create a new dashboard.
    pub fn new(store: Arc<dyn Store>, change_log: JsonlChangeLog) -> Self {
        Self { store, change_log }
    }

    /// Generate dashboard metrics.
    pub async fn generate_metrics(&self) -> DashboardMetrics {
        let typed_store = TypedMemoryStore::new(self.store.clone());
        let namespace = &["agent", "memories"];

        // Get all memories
        let all_memories = typed_store
            .list_typed(namespace, &MemoryFilter::new())
            .await
            .unwrap_or_default();

        // Calculate memory statistics by type
        let mut memory_by_type: HashMap<MemoryType, MemoryStats> = HashMap::new();
        for entry in &all_memories {
            let stats = memory_by_type
                .entry(entry.meta.memory_type)
                .or_insert_with(|| MemoryStats {
                    count: 0,
                    avg_confidence: 0.0,
                    active_count: 0,
                    archived_count: 0,
                });

            stats.count += 1;
            stats.avg_confidence += entry.meta.confidence;

            match entry.meta.status {
                MemoryStatus::Active => stats.active_count += 1,
                MemoryStatus::Archived => stats.archived_count += 1,
                _ => {}
            }
        }

        // Calculate averages
        for stats in memory_by_type.values_mut() {
            if stats.count > 0 {
                stats.avg_confidence /= stats.count as f32;
            }
        }

        // Calculate memory statistics by status
        let mut memory_by_status: HashMap<MemoryStatus, usize> = HashMap::new();
        for entry in &all_memories {
            *memory_by_status.entry(entry.meta.status).or_insert(0) += 1;
        }

        // Get skill health overview
        let skill_health = self.get_skill_health_overview().await;

        // Get recent activities from change log
        let recent_activities = self.get_recent_activities(10);

        DashboardMetrics {
            memory_by_type,
            memory_by_status,
            total_memories: all_memories.len(),
            skill_health,
            recent_activities,
            generated_at: Utc::now(),
        }
    }

    /// Get skill health overview from telemetry.
    async fn get_skill_health_overview(&self) -> SkillHealthOverview {
        let telemetry_store = SkillTelemetryStore::new(self.store.clone());
        let telemetries = telemetry_store.list_all().await.unwrap_or_default();

        if telemetries.is_empty() {
            return SkillHealthOverview {
                total_skills: 0,
                healthy_skills: 0,
                unhealthy_skills: 0,
                needs_attention: 0,
                avg_success_rate: 0.0,
            };
        }

        let mut healthy = 0;
        let mut unhealthy = 0;
        let mut needs_attention = 0;
        let mut total_success_rate = 0.0;

        for telemetry in &telemetries {
            let success_rate = telemetry.success_rate();
            total_success_rate += success_rate;

            if success_rate > 0.7 {
                healthy += 1;
            } else if success_rate < 0.5 {
                unhealthy += 1;
            } else {
                needs_attention += 1;
            }
        }

        let avg_success_rate = if telemetries.is_empty() {
            0.0
        } else {
            (total_success_rate / telemetries.len() as f64) as f32
        };

        SkillHealthOverview {
            total_skills: telemetries.len(),
            healthy_skills: healthy,
            unhealthy_skills: unhealthy,
            needs_attention,
            avg_success_rate,
        }
    }

    /// Get recent evolution activities from change log.
    fn get_recent_activities(&self, limit: usize) -> Vec<ActivityEntry> {
        let filter = echo_agent::evolution::ChangeFilter {
            limit: Some(limit),
            ..Default::default()
        };
        self.change_log
            .query(&filter)
            .unwrap_or_default()
            .into_iter()
            .map(|entry| ActivityEntry {
                activity_type: format!("{:?}", entry.change_type),
                description: entry.reason.clone(),
                timestamp: entry.timestamp,
            })
            .collect()
    }

    /// Format dashboard metrics as a human-readable string.
    pub fn format_metrics(metrics: &DashboardMetrics) -> String {
        let mut output = String::new();

        output.push_str("=== Evolution Dashboard ===\n\n");

        // Memory overview
        output.push_str(&format!(
            "📊 Total Memories: {}\n\n",
            metrics.total_memories
        ));

        if !metrics.memory_by_type.is_empty() {
            output.push_str("Memory by Type:\n");
            for (memory_type, stats) in &metrics.memory_by_type {
                output.push_str(&format!(
                    "  • {:?}: {} ({} active, {} archived, avg confidence: {:.2})\n",
                    memory_type,
                    stats.count,
                    stats.active_count,
                    stats.archived_count,
                    stats.avg_confidence
                ));
            }
            output.push('\n');
        }

        if !metrics.memory_by_status.is_empty() {
            output.push_str("Memory by Status:\n");
            for (status, count) in &metrics.memory_by_status {
                output.push_str(&format!("  • {:?}: {}\n", status, count));
            }
            output.push('\n');
        }

        // Skill health
        output.push_str("🎯 Skill Health:\n");
        if metrics.skill_health.total_skills > 0 {
            output.push_str(&format!(
                "  • Total Skills: {}\n",
                metrics.skill_health.total_skills
            ));
            output.push_str(&format!(
                "  • Healthy: {} (success rate > 70%)\n",
                metrics.skill_health.healthy_skills
            ));
            output.push_str(&format!(
                "  • Needs Attention: {} (success rate 50-70%)\n",
                metrics.skill_health.needs_attention
            ));
            output.push_str(&format!(
                "  • Unhealthy: {} (success rate < 50%)\n",
                metrics.skill_health.unhealthy_skills
            ));
            output.push_str(&format!(
                "  • Average Success Rate: {:.1}%\n\n",
                metrics.skill_health.avg_success_rate * 100.0
            ));
        } else {
            output.push_str("  • No skill telemetry available\n\n");
        }

        // Recent activities
        if !metrics.recent_activities.is_empty() {
            output.push_str("📝 Recent Activities:\n");
            for activity in &metrics.recent_activities {
                output.push_str(&format!(
                    "  • [{}] {} - {}\n",
                    activity.timestamp.format("%Y-%m-%d %H:%M"),
                    activity.activity_type,
                    activity.description
                ));
            }
            output.push('\n');
        }

        output.push_str(&format!(
            "Generated at: {}\n",
            metrics.generated_at.format("%Y-%m-%d %H:%M:%S UTC")
        ));

        output
    }
}
