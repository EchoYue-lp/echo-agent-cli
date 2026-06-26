//! Runtime-owned worker delegation planning.
//!
//! This is the deterministic Phase 4 path: broad read-only work is mapped to
//! worker specs by the runtime, not by asking the chat model to call
//! `agent_tool`. Domain profiles are only hints; capability signals decide the
//! worker mix so coding, data, academic, and medical abilities can blend.

use super::profiles::{ALL_WORKER_ROLES, ProfileTemplate, WORKER_CAPABILITY_CATALOG};
use super::signals::{CapabilityArea, RoutingSignals};
use super::types::DomainProfile;

pub const DEFAULT_MAX_READONLY_WORKERS: usize = 11;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSpec {
    pub agent_name: String,
    pub title: String,
    pub task: String,
    pub purpose: String,
    pub readonly: bool,
    pub expected_output: String,
    /// Agent names this worker should wait for. The planner rewrites these
    /// into concrete plan task ids when creating the DAG.
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DelegationRequest<'a> {
    pub goal: &'a str,
    pub profile: DomainProfile,
    pub signals: &'a RoutingSignals,
    pub suggested_workers: &'a [String],
    pub max_workers: usize,
}

pub struct DelegationPlanner;

impl DelegationPlanner {
    pub fn plan_readonly(request: DelegationRequest<'_>) -> Vec<WorkerSpec> {
        let max_workers = request.max_workers.max(1);
        let mut roles = Vec::new();

        for worker in request.suggested_workers {
            push_role(&mut roles, worker);
        }

        push_area_roles_round_robin(&mut roles, &request.signals.capability_areas);

        if roles.is_empty() {
            let template = ProfileTemplate::for_profile(request.profile);
            for role in template.default_worker_roles {
                push_role(&mut roles, role);
            }
        }

        if roles.is_empty() {
            push_roles(&mut roles, &["explorer", "reviewer", "planner"]);
        }

        let mut discovery_roles = roles
            .into_iter()
            .filter(|role| role != "summarizer")
            .collect::<Vec<_>>();

        if discovery_roles.is_empty() {
            discovery_roles.push("explorer".to_string());
        }

        let include_summary = discovery_roles.len() > 1;
        let discovery_limit = if include_summary && max_workers > 1 {
            max_workers.saturating_sub(1)
        } else {
            max_workers
        }
        .max(1);
        discovery_roles.truncate(discovery_limit);

        let mut specs = discovery_roles
            .iter()
            .map(|role| WorkerSpec {
                agent_name: role.clone(),
                title: worker_title(role),
                task: worker_task(role, request.goal),
                purpose: worker_purpose(role),
                readonly: true,
                expected_output: worker_expected_output(role),
                depends_on: Vec::new(),
            })
            .collect::<Vec<_>>();

        if include_summary && specs.len() < max_workers {
            let depends_on = specs
                .iter()
                .map(|spec| spec.agent_name.clone())
                .collect::<Vec<_>>();
            specs.push(WorkerSpec {
                agent_name: "summarizer".to_string(),
                title: worker_title("summarizer"),
                task: format!(
                    "Synthesize all worker findings into a direct answer for this goal: {}",
                    request.goal.trim()
                ),
                purpose: worker_purpose("summarizer"),
                readonly: true,
                expected_output: "A concise synthesis with agreements, conflicts, evidence gaps, and next actions.".to_string(),
                depends_on,
            });
        }

        specs
    }
}

pub fn worker_role_names(specs: &[WorkerSpec]) -> Vec<String> {
    specs.iter().map(|spec| spec.agent_name.clone()).collect()
}

pub fn role_slug(role: &str) -> String {
    role.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn roles_for_area(area: &CapabilityArea) -> &'static [&'static str] {
    // SA-4: All areas map to the same 3 generic discovery roles.
    // Domain specialization is via skill/profile injection, not agent type.
    let _ = area; // area still logged for telemetry but no longer selects different roles
    &["explorer", "reviewer", "planner"]
}

fn push_roles(out: &mut Vec<String>, workers: &[&str]) {
    for worker in workers {
        push_role(out, worker);
    }
}

fn push_area_roles_round_robin(out: &mut Vec<String>, areas: &[CapabilityArea]) {
    let role_sets = areas.iter().map(roles_for_area).collect::<Vec<_>>();
    let max_len = role_sets.iter().map(|roles| roles.len()).max().unwrap_or(0);
    for index in 0..max_len {
        for roles in &role_sets {
            if let Some(role) = roles.get(index) {
                push_role(out, role);
            }
        }
    }
}

fn push_role(out: &mut Vec<String>, worker: &str) {
    if ALL_WORKER_ROLES.iter().any(|allowed| allowed == &worker)
        && !out.iter().any(|existing| existing == worker)
    {
        out.push(worker.to_string());
    }
}

fn worker_title(worker: &str) -> String {
    match worker {
        "explorer" => "Explore and map the target domain".to_string(),
        "reviewer" => "Review for risks, quality, and gaps".to_string(),
        "planner" => "Plan verification and next steps".to_string(),
        "summarizer" => "Synthesize worker findings".to_string(),
        _ => format!("Run read-only worker {}", worker),
    }
}

fn worker_task(worker: &str, goal: &str) -> String {
    format!(
        "{} Focus on this goal: {}",
        worker_description(worker),
        goal.trim()
    )
}

fn worker_purpose(worker: &str) -> String {
    worker_description(worker)
}

fn worker_expected_output(worker: &str) -> String {
    if worker == "summarizer" {
        return "Synthesis grounded in completed worker outputs, with uncertainty and next actions.".to_string();
    }
    format!(
        "Concrete findings from {worker}, including evidence, file/source references, uncertainty, and risks."
    )
}

fn worker_description(worker: &str) -> String {
    WORKER_CAPABILITY_CATALOG
        .iter()
        .find(|(role, _)| role == &worker)
        .map(|(_, capability)| format!("Use {} capability: {}.", worker, capability))
        .unwrap_or_else(|| format!("Use {} for read-only investigation.", worker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_analysis_uses_variable_worker_count_with_summary() {
        let signals = RoutingSignals::analyze(DomainProfile::AiCoding, "分析当前项目架构");
        let specs = DelegationPlanner::plan_readonly(DelegationRequest {
            goal: "分析当前项目架构",
            profile: DomainProfile::AiCoding,
            signals: &signals,
            suggested_workers: &[],
            max_workers: 4,
        });

        assert!(!specs.is_empty());
        assert!(specs.iter().any(|spec| spec.agent_name == "explorer"));
        assert!(specs.iter().any(|spec| spec.agent_name == "summarizer"));
    }

    #[test]
    fn blended_medical_data_code_task_crosses_domain_boundaries() {
        let goal = "分析医学 cohort 数据集和 Python notebook，审查临床证据和复现代码";
        let signals = RoutingSignals::analyze(DomainProfile::MedicalResearch, goal);
        let specs = DelegationPlanner::plan_readonly(DelegationRequest {
            goal,
            profile: DomainProfile::MedicalResearch,
            signals: &signals,
            suggested_workers: &[],
            max_workers: 10,
        });
        let roles = worker_role_names(&specs);

        // SA-4: all domains now use the 4 generic roles.
        for expected in ["explorer", "reviewer", "planner", "summarizer"] {
            assert!(
                roles.iter().any(|role| role == expected),
                "expected {expected}, got {:?}",
                roles
            );
        }
    }

    #[test]
    fn max_workers_caps_discovery_but_keeps_summary_when_possible() {
        let goal = "review papers data evidence code and medical safety";
        let signals = RoutingSignals::analyze(DomainProfile::MedicalResearch, goal);
        let specs = DelegationPlanner::plan_readonly(DelegationRequest {
            goal,
            profile: DomainProfile::MedicalResearch,
            signals: &signals,
            suggested_workers: &[],
            max_workers: 3,
        });

        assert_eq!(specs.len(), 3);
        assert_eq!(
            specs.last().map(|spec| spec.agent_name.as_str()),
            Some("summarizer")
        );
    }
}
