//! Evolution dashboard — provides metrics and status overview.
//!
//! The dashboard aggregates evolution system metrics including:
//! - Memory statistics by type and status
//! - Recent evolution activities
//! - Repeated tool failures across multiple runs, only on explicit request

use chrono::{DateTime, Utc};
use echo_agent::evolution::{ChangeLog, JsonlChangeLog};
use echo_agent::memory::{MemoryFilter, MemoryStatus, MemoryType, Store, TypedMemoryStore};
use echo_agent::trace::{RunStore, ToolFailurePattern, TraceAnalyzer};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

const MAX_DIAGNOSTIC_RUNS: usize = 200;
const MAX_REPEATED_FAILURE_REMINDERS: usize = 3;

/// Dashboard metrics summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardMetrics {
    /// Memory statistics by type.
    pub memory_by_type: HashMap<MemoryType, MemoryStats>,
    /// Memory statistics by status.
    pub memory_by_status: HashMap<MemoryStatus, usize>,
    /// Total number of memories.
    pub total_memories: usize,
    /// On-demand repeated tool failures derived without LLM calls.
    pub tool_diagnostics: ToolDiagnostics,
    /// Recent evolution activities (last 10).
    pub recent_activities: Vec<ActivityEntry>,
    /// Timestamp when metrics were generated.
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
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

/// Small diagnostic projection computed only when the user opens the dashboard.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolDiagnostics {
    pub repeated_failures: Vec<ToolFailurePattern>,
    pub scan_error: Option<String>,
}

/// An evolution activity entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    /// Type of activity.
    pub activity_type: String,
    /// Description of the activity.
    pub description: String,
    /// When the activity occurred.
    #[serde(with = "echo_agent::utils::time::local_rfc3339")]
    pub timestamp: DateTime<Utc>,
}

/// Dashboard for evolution system metrics.
pub struct Dashboard {
    store: Arc<dyn Store>,
    change_log: JsonlChangeLog,
    run_store: Option<Arc<dyn RunStore>>,
}

impl Dashboard {
    /// Create a new dashboard.
    pub fn new(store: Arc<dyn Store>, change_log: JsonlChangeLog) -> Self {
        Self {
            store,
            change_log,
            run_store: None,
        }
    }

    /// Attach the existing trace store for explicit, on-demand diagnostics.
    pub fn with_run_store(mut self, run_store: Option<Arc<dyn RunStore>>) -> Self {
        self.run_store = run_store;
        self
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

        let tool_diagnostics = self.get_tool_diagnostics().await;

        // Get recent activities from change log
        let recent_activities = self.get_recent_activities(10);

        DashboardMetrics {
            memory_by_type,
            memory_by_status,
            total_memories: all_memories.len(),
            tool_diagnostics,
            recent_activities,
            generated_at: Utc::now(),
        }
    }

    async fn get_tool_diagnostics(&self) -> ToolDiagnostics {
        let Some(run_store) = self.run_store.as_ref() else {
            return ToolDiagnostics::default();
        };
        match TraceAnalyzer::new(run_store.clone())
            .tool_reliability_report(MAX_DIAGNOSTIC_RUNS, None)
            .await
        {
            Ok(report) => tool_diagnostics_from_report(report),
            Err(error) => ToolDiagnostics {
                repeated_failures: Vec::new(),
                scan_error: Some(error.to_string()),
            },
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

        if !metrics.tool_diagnostics.repeated_failures.is_empty() {
            output.push_str("Repeated Tool Failures:\n");
            for failure in &metrics.tool_diagnostics.repeated_failures {
                output.push_str(&format!(
                    "  • {}: {:?}, {} occurrences across {} runs\n",
                    failure.tool_name,
                    failure.error_class,
                    failure.occurrence_count,
                    failure.distinct_run_count
                ));
            }
            output.push('\n');
        } else if let Some(error) = &metrics.tool_diagnostics.scan_error {
            output.push_str(&format!("Tool diagnostics unavailable: {error}\n\n"));
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

fn tool_diagnostics_from_report(
    report: echo_agent::trace::ToolReliabilityReport,
) -> ToolDiagnostics {
    let repeated_failures = report
        .failure_patterns
        .into_iter()
        .filter(|pattern| pattern.occurrence_count >= 3 && pattern.distinct_run_count >= 2)
        .take(MAX_REPEATED_FAILURE_REMINDERS)
        .collect();
    ToolDiagnostics {
        repeated_failures,
        scan_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::trace::{ToolFailureClass, ToolReliabilityReport};

    #[test]
    fn diagnostics_require_cross_run_repetition() {
        let repeated = ToolFailurePattern {
            tool_name: "apply_patch".to_string(),
            error_class: ToolFailureClass::Permanent,
            pattern: "permission denied".to_string(),
            input_shape: "object{path:string}".to_string(),
            occurrence_count: 3,
            distinct_run_count: 2,
            run_ids: vec!["r1".to_string(), "r2".to_string()],
            ineffective_retry_count: 1,
            first_seen: Some(Utc::now()),
            last_seen: Some(Utc::now()),
        };
        let one_run = ToolFailurePattern {
            distinct_run_count: 1,
            run_ids: vec!["r3".to_string()],
            ..repeated.clone()
        };
        let diagnostics = tool_diagnostics_from_report(ToolReliabilityReport {
            run_count: 3,
            total_calls: 10,
            success_count: 6,
            failure_count: 4,
            ineffective_retry_count: 2,
            failure_patterns: vec![repeated, one_run],
        });

        assert_eq!(diagnostics.repeated_failures.len(), 1);
        assert_eq!(
            diagnostics
                .repeated_failures
                .first()
                .map(|failure| failure.distinct_run_count),
            Some(2)
        );
        assert_eq!(diagnostics.scan_error, None);
    }
}
