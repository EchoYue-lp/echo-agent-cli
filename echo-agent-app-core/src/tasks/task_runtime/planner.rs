//! Lightweight DAG integrity checks for plan task graphs.
//!
//! Contains `validate_plan_deps` — a standalone function to verify that a set
//! of plan tasks has no dangling dependencies and no cycles. This is used by
//! the runtime store when tasks are inserted or updated dynamically.
//!
//! Previous plan generation functions (`generate_parallel_readonly_plan`,
//! `generate_plan`) have been removed as part of the L1 path cleanup. Plans
//! are now produced by the main agent ReAct loop via `task_create`.

/// Validate dependency integrity and acyclicity for a set of tasks.
///
/// This is a lightweight check used by dynamic plan operations (insert_task,
/// update_task) to ensure the DAG remains valid after mutation. Unlike
/// `validate_plan`, it skips structural quality checks (file lists, title
/// length, etc.) and only verifies:
///
/// 1. Every `depends_on` references an existing task id.
/// 2. The dependency graph has no cycles.
pub fn validate_plan_deps(tasks: &[super::types::PlanTask]) -> Result<(), Vec<String>> {
    let mut errors: Vec<String> = Vec::new();

    // 1. Dangling dependency check.
    let ids: std::collections::HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    for t in tasks {
        for dep in &t.depends_on {
            if !ids.contains(dep.as_str()) {
                errors.push(format!(
                    "task '{}' depends on '{}' which does not exist",
                    t.id, dep
                ));
            }
        }
    }

    // 2. Cycle detection via DFS.
    {
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut stack: std::collections::HashSet<String> = std::collections::HashSet::new();
        let id_to_deps: std::collections::HashMap<String, Vec<String>> = tasks
            .iter()
            .map(|t| (t.id.clone(), t.depends_on.clone()))
            .collect();
        fn dfs(
            node: &str,
            id_to_deps: &std::collections::HashMap<String, Vec<String>>,
            visited: &mut std::collections::HashSet<String>,
            stack: &mut std::collections::HashSet<String>,
        ) -> bool {
            if stack.contains(node) {
                return true;
            }
            if visited.contains(node) {
                return false;
            }
            visited.insert(node.to_string());
            stack.insert(node.to_string());
            if let Some(deps) = id_to_deps.get(node) {
                for dep in deps {
                    if dfs(dep, id_to_deps, visited, stack) {
                        return true;
                    }
                }
            }
            stack.remove(node);
            false
        }
        for t in tasks {
            if visited.contains(&t.id) {
                continue;
            }
            if dfs(&t.id, &id_to_deps, &mut visited, &mut stack) {
                errors.push(format!("dependency cycle involving task '{}'", t.id));
                break;
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
