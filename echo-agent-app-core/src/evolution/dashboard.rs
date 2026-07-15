//! Evolution dashboard — provides metrics and status overview.
//!
//! The dashboard aggregates evolution system metrics including:
//! - Memory statistics by type and status
//! - Skill health overview
//! - Recent evolution activities
//! - Promotion candidates

use chrono::{DateTime, Duration, Utc};
use echo_agent::evolution::{ChangeLog, ChangeType, EntityType, JsonlChangeLog};
use echo_agent::memory::{MemoryFilter, MemoryStatus, MemoryType, Store, TypedMemoryStore};
use echo_agent::trace::{RunStore, ToolFailurePattern, TraceAnalyzer};
use echo_agent::workspace::state::skill_telemetry::SkillTelemetryStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use super::{EvidenceFeedbackMetrics, EvidenceStore};

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
    /// Real usage feedback derived without LLM calls.
    pub real_usage: RealUsageMetrics,
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

/// Skill health overview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillHealthOverview {
    /// Total number of skills with telemetry.
    pub total_skills: usize,
    /// Skills whose tool calls succeed more than 70% while active.
    pub reliable_skills: usize,
    /// Skills whose tool calls succeed less than 50% while active.
    pub unreliable_skills: usize,
    /// Skills whose tool-call success rate is between 50% and 70%.
    pub needs_attention: usize,
    /// Total tool-call observations recorded while a skill was active.
    pub observed_tool_calls: u64,
    /// Successful tool-call observations while a skill was active.
    pub successful_tool_calls: u64,
    /// Failed tool-call observations while a skill was active.
    pub failed_tool_calls: u64,
    /// Average tool-call success rate across skills.
    pub avg_tool_success_rate: f32,
}

/// Real usage feedback in the three product time windows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RealUsageMetrics {
    pub last_7_days: FeedbackWindowMetrics,
    pub last_30_days: FeedbackWindowMetrics,
    pub all_time: FeedbackWindowMetrics,
}

/// Cross-source metrics for one time window.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FeedbackWindowMetrics {
    pub evidence: EvidenceFeedbackMetrics,
    pub audit: AuditFeedbackMetrics,
    pub tools: ToolFeedbackMetrics,
    /// Data source errors are explicit instead of silently reporting zeroes.
    pub data_errors: Vec<String>,
}

/// Mutation counts from the append-only evolution audit log.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditFeedbackMetrics {
    pub total_mutations: usize,
    pub by_entity: HashMap<EntityType, usize>,
    pub by_change: HashMap<ChangeType, usize>,
    pub by_trigger: HashMap<String, usize>,
    pub reviewed_mutations: usize,
    pub automatic_maintenance_mutations: usize,
}

/// Tool reliability derived from execution traces.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolFeedbackMetrics {
    pub run_count: usize,
    pub total_calls: usize,
    pub success_count: usize,
    pub failure_count: usize,
    pub failure_rate: Option<f32>,
    pub repeated_failure_patterns: usize,
    pub ineffective_retry_count: usize,
    pub top_repeated_failures: Vec<ToolFailurePattern>,
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
    evidence_store: Option<EvidenceStore>,
    run_store: Option<Arc<dyn RunStore>>,
}

impl Dashboard {
    /// Create a new dashboard.
    pub fn new(store: Arc<dyn Store>, change_log: JsonlChangeLog) -> Self {
        Self {
            store,
            change_log,
            evidence_store: None,
            run_store: None,
        }
    }

    /// Attach EKO's existing Evidence JSONL and execution trace sources.
    pub fn with_feedback_sources(
        mut self,
        evidence_store: EvidenceStore,
        run_store: Option<Arc<dyn RunStore>>,
    ) -> Self {
        self.evidence_store = Some(evidence_store);
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

        // Get skill health overview
        let skill_health = self.get_skill_health_overview().await;

        let real_usage = self.get_real_usage_metrics().await;

        // Get recent activities from change log
        let recent_activities = self.get_recent_activities(10);

        DashboardMetrics {
            memory_by_type,
            memory_by_status,
            total_memories: all_memories.len(),
            skill_health,
            real_usage,
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
                reliable_skills: 0,
                unreliable_skills: 0,
                needs_attention: 0,
                observed_tool_calls: 0,
                successful_tool_calls: 0,
                failed_tool_calls: 0,
                avg_tool_success_rate: 0.0,
            };
        }

        let mut healthy = 0;
        let mut unhealthy = 0;
        let mut needs_attention = 0;
        let mut total_success_rate = 0.0;
        let mut observed_tool_calls = 0u64;
        let mut successful_tool_calls = 0u64;
        let mut failed_tool_calls = 0u64;

        for telemetry in &telemetries {
            let success_rate = telemetry.success_rate();
            total_success_rate += success_rate;
            observed_tool_calls = observed_tool_calls.saturating_add(telemetry.activation_count);
            successful_tool_calls = successful_tool_calls.saturating_add(telemetry.success_count);
            failed_tool_calls = failed_tool_calls.saturating_add(telemetry.failure_count);

            if success_rate > 0.7 {
                healthy += 1;
            } else if success_rate < 0.5 {
                unhealthy += 1;
            } else {
                needs_attention += 1;
            }
        }

        let avg_tool_success_rate = if telemetries.is_empty() {
            0.0
        } else {
            (total_success_rate / telemetries.len() as f64) as f32
        };

        SkillHealthOverview {
            total_skills: telemetries.len(),
            reliable_skills: healthy,
            unreliable_skills: unhealthy,
            needs_attention,
            observed_tool_calls,
            successful_tool_calls,
            failed_tool_calls,
            avg_tool_success_rate,
        }
    }

    async fn get_real_usage_metrics(&self) -> RealUsageMetrics {
        let now = Utc::now();
        RealUsageMetrics {
            last_7_days: self
                .get_feedback_window(Some(now - Duration::days(7)))
                .await,
            last_30_days: self
                .get_feedback_window(Some(now - Duration::days(30)))
                .await,
            all_time: self.get_feedback_window(None).await,
        }
    }

    async fn get_feedback_window(&self, after: Option<DateTime<Utc>>) -> FeedbackWindowMetrics {
        let mut data_errors = Vec::new();
        let evidence = match self.evidence_store.as_ref() {
            Some(store) => match store.feedback_metrics(after) {
                Ok(metrics) => metrics,
                Err(error) => {
                    data_errors.push(format!("evidence: {error}"));
                    EvidenceFeedbackMetrics::default()
                }
            },
            None => EvidenceFeedbackMetrics::default(),
        };

        let audit = match self.change_log.query(&echo_agent::evolution::ChangeFilter {
            after,
            ..Default::default()
        }) {
            Ok(entries) => aggregate_audit_metrics(entries),
            Err(error) => {
                data_errors.push(format!("audit: {error}"));
                AuditFeedbackMetrics::default()
            }
        };

        let tools = match self.run_store.as_ref() {
            Some(run_store) => {
                match TraceAnalyzer::new(run_store.clone())
                    .tool_reliability_report(usize::MAX, after)
                    .await
                {
                    Ok(report) => tool_feedback_metrics(report),
                    Err(error) => {
                        data_errors.push(format!("trace: {error}"));
                        ToolFeedbackMetrics::default()
                    }
                }
            }
            None => ToolFeedbackMetrics::default(),
        };

        FeedbackWindowMetrics {
            evidence,
            audit,
            tools,
            data_errors,
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

        // Skill health. Current telemetry is tool-level while a skill is active,
        // not task-level outcome attribution.
        output.push_str("Skill Tool Reliability:\n");
        if metrics.skill_health.total_skills > 0 {
            output.push_str(&format!(
                "  • Total Skills: {}\n",
                metrics.skill_health.total_skills
            ));
            output.push_str(&format!(
                "  • Reliable: {} (tool success rate > 70%)\n",
                metrics.skill_health.reliable_skills
            ));
            output.push_str(&format!(
                "  • Needs Attention: {} (tool success rate 50-70%)\n",
                metrics.skill_health.needs_attention
            ));
            output.push_str(&format!(
                "  • Unreliable: {} (tool success rate < 50%)\n",
                metrics.skill_health.unreliable_skills
            ));
            output.push_str(&format!(
                "  • Tool Observations: {} ({} succeeded, {} failed)\n",
                metrics.skill_health.observed_tool_calls,
                metrics.skill_health.successful_tool_calls,
                metrics.skill_health.failed_tool_calls
            ));
            output.push_str(&format!(
                "  • Average Tool Success Rate: {:.1}%\n\n",
                metrics.skill_health.avg_tool_success_rate * 100.0
            ));
        } else {
            output.push_str("  • No skill telemetry available\n\n");
        }

        output.push_str("Real Usage Feedback:\n");
        append_feedback_window(
            &mut output,
            "Last 30 days",
            &metrics.real_usage.last_30_days,
        );
        append_feedback_window(&mut output, "All time", &metrics.real_usage.all_time);
        output.push('\n');

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

fn append_feedback_window(output: &mut String, label: &str, window: &FeedbackWindowMetrics) {
    output.push_str(&format!("  {label}:\n"));
    output.push_str(&format!(
        "    candidates {} accepted / {} rejected / {} undone; {} stale proposals\n",
        window.evidence.accepted_candidates,
        window.evidence.rejected_candidates,
        window.evidence.undone_candidates,
        window.evidence.stale_proposal_failures
    ));
    output.push_str(&format!(
        "    decision rates accept {} / reject {} / undo {}\n",
        format_rate(window.evidence.acceptance_rate),
        format_rate(window.evidence.rejection_rate),
        format_rate(window.evidence.undo_rate)
    ));
    output.push_str(&format!(
        "    tools {} calls / {} failures / {} repeated patterns / {} ineffective retries\n",
        window.tools.total_calls,
        window.tools.failure_count,
        window.tools.repeated_failure_patterns,
        window.tools.ineffective_retry_count
    ));
    output.push_str(&format!(
        "    audit {} mutations ({} reviewed, {} automatic maintenance)\n",
        window.audit.total_mutations,
        window.audit.reviewed_mutations,
        window.audit.automatic_maintenance_mutations
    ));
    if !window.data_errors.is_empty() {
        output.push_str(&format!(
            "    data errors: {}\n",
            window.data_errors.join("; ")
        ));
    }
}

fn format_rate(rate: Option<f32>) -> String {
    rate.map(|value| format!("{:.1}%", value * 100.0))
        .unwrap_or_else(|| "insufficient sample".to_string())
}

fn aggregate_audit_metrics(
    entries: Vec<echo_agent::evolution::ChangeEntry>,
) -> AuditFeedbackMetrics {
    let mut metrics = AuditFeedbackMetrics::default();
    for entry in entries {
        metrics.total_mutations = metrics.total_mutations.saturating_add(1);
        *metrics.by_entity.entry(entry.entity_type).or_insert(0) += 1;
        *metrics.by_change.entry(entry.change_type).or_insert(0) += 1;
        *metrics.by_trigger.entry(entry.trigger.clone()).or_insert(0) += 1;
        if entry.trigger.starts_with("review_inbox")
            || entry.trigger == "explicit_memory_merge"
            || entry.trigger.contains("user")
            || entry.trigger.contains("command")
            || entry.entity_key.starts_with("evidence_ec_")
        {
            metrics.reviewed_mutations = metrics.reviewed_mutations.saturating_add(1);
        }
        if matches!(
            entry.trigger.as_str(),
            "dreaming" | "promote" | "demote" | "enforce_hot_budget"
        ) || entry.trigger.starts_with("auto_")
        {
            metrics.automatic_maintenance_mutations =
                metrics.automatic_maintenance_mutations.saturating_add(1);
        }
    }
    metrics
}

fn tool_feedback_metrics(report: echo_agent::trace::ToolReliabilityReport) -> ToolFeedbackMetrics {
    let repeated = report
        .failure_patterns
        .into_iter()
        .filter(|pattern| pattern.occurrence_count >= 3 && pattern.distinct_run_count >= 2)
        .collect::<Vec<_>>();
    let failure_rate = (report.total_calls >= 10)
        .then_some(report.failure_count as f32 / report.total_calls as f32);
    ToolFeedbackMetrics {
        run_count: report.run_count,
        total_calls: report.total_calls,
        success_count: report.success_count,
        failure_count: report.failure_count,
        failure_rate,
        repeated_failure_patterns: repeated.len(),
        ineffective_retry_count: report.ineffective_retry_count,
        top_repeated_failures: repeated.into_iter().take(5).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent::trace::{ToolFailureClass, ToolReliabilityReport};

    fn change(trigger: &str, key: &str) -> echo_agent::evolution::ChangeEntry {
        echo_agent::evolution::ChangeEntry {
            change_id: format!("change-{trigger}-{key}"),
            timestamp: Utc::now(),
            entity_type: EntityType::Memory,
            entity_key: key.to_string(),
            change_type: ChangeType::Create,
            before: None,
            after: None,
            reason: "test".to_string(),
            trigger: trigger.to_string(),
        }
    }

    #[test]
    fn audit_metrics_separate_reviewed_and_automatic_changes() {
        let metrics = aggregate_audit_metrics(vec![
            change("write_memory", "evidence_ec_1"),
            change("explicit_memory_merge", "memory_a"),
            change("dreaming", "memory_b"),
            change("promote", "memory_c"),
        ]);

        assert_eq!(metrics.total_mutations, 4);
        assert_eq!(metrics.reviewed_mutations, 2);
        assert_eq!(metrics.automatic_maintenance_mutations, 2);
        assert_eq!(
            metrics
                .by_entity
                .get(&EntityType::Memory)
                .copied()
                .unwrap_or(0),
            4
        );
    }

    #[test]
    fn tool_metrics_require_cross_run_repetition() {
        let repeated = ToolFailurePattern {
            tool_name: "write_file".to_string(),
            error_class: ToolFailureClass::PermissionDenied,
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
        let metrics = tool_feedback_metrics(ToolReliabilityReport {
            run_count: 3,
            total_calls: 10,
            success_count: 6,
            failure_count: 4,
            ineffective_retry_count: 2,
            failure_patterns: vec![repeated, one_run],
        });

        assert_eq!(metrics.repeated_failure_patterns, 1);
        assert_eq!(metrics.top_repeated_failures.len(), 1);
        assert_eq!(metrics.failure_rate, Some(0.4));
    }
}
