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
            push_roles(
                &mut roles,
                &["project_explorer", "literature_scout", "data_profiler"],
            );
        }

        let mut discovery_roles = roles
            .into_iter()
            .filter(|role| role != "summary_writer")
            .collect::<Vec<_>>();

        if discovery_roles.is_empty() {
            discovery_roles.push("project_explorer".to_string());
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
                agent_name: "summary_writer".to_string(),
                title: worker_title("summary_writer"),
                task: format!(
                    "Synthesize all worker findings into a direct answer for this goal: {}",
                    request.goal.trim()
                ),
                purpose: worker_purpose("summary_writer"),
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
    match area {
        CapabilityArea::Coding => &["project_explorer", "code_reviewer", "test_planner"],
        CapabilityArea::Data => &[
            "data_profiler",
            "analysis_reviewer",
            "reproducibility_planner",
        ],
        CapabilityArea::Academic => &["literature_scout", "evidence_reviewer", "synthesis_planner"],
        CapabilityArea::Medical => &[
            "medical_literature_scout",
            "clinical_evidence_reviewer",
            "safety_reviewer",
        ],
    }
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
        "project_explorer" => "Explore project structure and architecture".to_string(),
        "code_reviewer" => "Review code architecture and correctness risks".to_string(),
        "test_planner" => "Map verification and test strategy".to_string(),
        "data_profiler" => "Profile data sources and analysis shape".to_string(),
        "analysis_reviewer" => "Review metrics and analytical assumptions".to_string(),
        "reproducibility_planner" => "Plan reproducibility checks".to_string(),
        "literature_scout" => "Scout relevant literature and sources".to_string(),
        "evidence_reviewer" => "Review evidence quality and claim strength".to_string(),
        "synthesis_planner" => "Plan research synthesis structure".to_string(),
        "medical_literature_scout" => "Scout medical literature and guidelines".to_string(),
        "clinical_evidence_reviewer" => "Review clinical evidence and applicability".to_string(),
        "safety_reviewer" => "Review safety boundaries and risk notes".to_string(),
        "summary_writer" => "Synthesize worker findings".to_string(),
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
    if worker == "summary_writer" {
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

        assert_eq!(specs.len(), 4);
        assert!(
            specs
                .iter()
                .any(|spec| spec.agent_name == "project_explorer")
        );
        assert!(specs.iter().any(|spec| spec.agent_name == "code_reviewer"));
        assert!(specs.iter().any(|spec| spec.agent_name == "summary_writer"));
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

        for expected in [
            "medical_literature_scout",
            "clinical_evidence_reviewer",
            "data_profiler",
            "analysis_reviewer",
            "code_reviewer",
            "summary_writer",
        ] {
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
            Some("summary_writer")
        );
    }
}
