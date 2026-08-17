pub const TASK_GOAL_USAGE: &str =
    "/task-goal <expected-revision> [run-id] --reason <reason> --goal <new-goal>";
pub const REQUIREMENT_SKIP_USAGE: &str =
    "/task-requirement-skip <expected-goal-revision> <requirement-id> [run-id] --reason <reason>";
pub const SUBAGENT_MESSAGE_USAGE: &str = "/subagent-message <run-id> <task-id> <execution-id> <plan-revision> <attempt> <command-id> <instruction>";
pub const SUBAGENT_FOLLOWUP_USAGE: &str = "/subagent-followup <run-id> <task-id> <execution-id> <plan-revision> <attempt> <command-id> <instruction>";
pub const SUBAGENT_INTERRUPT_USAGE: &str =
    "/subagent-interrupt <run-id> <task-id> <execution-id> <plan-revision> <attempt> <command-id>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRunGoalUpdate {
    pub expected_goal_revision: u64,
    pub requested_run_id: Option<String>,
    pub reason: String,
    pub new_goal: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSubagentControl {
    pub identity: echo_agent_app_core::tasks::task_runtime::SubagentControlIdentity,
    pub instruction: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRequirementSkip {
    pub expected_goal_revision: u64,
    pub requirement_id: String,
    pub requested_run_id: Option<String>,
    pub reason: String,
}

pub fn parse_subagent_control_args(
    args: &[&str],
    usage: &str,
    instruction_required: bool,
) -> Result<ParsedSubagentControl, String> {
    let run_id = required_arg(args, 0, usage)?;
    let task_id = required_arg(args, 1, usage)?;
    let execution_id = required_arg(args, 2, usage)?;
    let plan_revision = required_arg(args, 3, usage)?
        .parse::<u64>()
        .map_err(|error| format!("invalid plan revision: {error}"))?;
    let attempt = required_arg(args, 4, usage)?
        .parse::<u32>()
        .map_err(|error| format!("invalid Subagent attempt: {error}"))?;
    let command_id = required_arg(args, 5, usage)?;
    if plan_revision == 0 || attempt == 0 {
        return Err("plan revision and Subagent attempt must be positive".to_string());
    }
    let instruction = args
        .get(6..)
        .unwrap_or_default()
        .join(" ")
        .trim()
        .to_string();
    if instruction_required && instruction.is_empty() {
        return Err(format!("Usage: {usage}"));
    }
    if !instruction_required && !instruction.is_empty() {
        return Err(format!("Usage: {usage}"));
    }
    Ok(ParsedSubagentControl {
        identity: echo_agent_app_core::tasks::task_runtime::SubagentControlIdentity {
            run_id: run_id.to_string(),
            task_id: task_id.to_string(),
            execution_id: execution_id.to_string(),
            plan_revision,
            attempt,
            command_id: command_id.to_string(),
        },
        instruction: instruction_required.then_some(instruction),
    })
}

fn required_arg<'a>(args: &'a [&str], index: usize, usage: &str) -> Result<&'a str, String> {
    args.get(index)
        .copied()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("Usage: {usage}"))
}

pub fn parse_run_goal_update_args(args: &[&str]) -> Result<ParsedRunGoalUpdate, String> {
    let revision = args
        .first()
        .ok_or_else(|| format!("Usage: {TASK_GOAL_USAGE}"))?
        .parse::<u64>()
        .map_err(|error| format!("invalid expected Goal revision: {error}"))?;
    if revision == 0 {
        return Err("expected Goal revision must be positive".to_string());
    }

    let mut cursor = 1;
    let requested_run_id = match args.get(cursor).copied() {
        Some("--reason") => None,
        Some(run_id) if !run_id.starts_with("--") => {
            cursor = cursor.saturating_add(1);
            Some(run_id.to_string())
        }
        _ => return Err(format!("Usage: {TASK_GOAL_USAGE}")),
    };
    if args.get(cursor).copied() != Some("--reason") {
        return Err(format!("Usage: {TASK_GOAL_USAGE}"));
    }
    cursor = cursor.saturating_add(1);
    let goal_marker = args
        .iter()
        .enumerate()
        .skip(cursor)
        .find_map(|(index, value)| (*value == "--goal").then_some(index))
        .ok_or_else(|| format!("Usage: {TASK_GOAL_USAGE}"))?;
    let reason = args.get(cursor..goal_marker).unwrap_or_default().join(" ");
    let new_goal = args
        .get(goal_marker.saturating_add(1)..)
        .unwrap_or_default()
        .join(" ");
    if reason.trim().is_empty() || new_goal.trim().is_empty() {
        return Err(format!("Usage: {TASK_GOAL_USAGE}"));
    }
    Ok(ParsedRunGoalUpdate {
        expected_goal_revision: revision,
        requested_run_id,
        reason,
        new_goal,
    })
}

pub fn parse_requirement_skip_args(args: &[&str]) -> Result<ParsedRequirementSkip, String> {
    let expected_goal_revision = required_arg(args, 0, REQUIREMENT_SKIP_USAGE)?
        .parse::<u64>()
        .map_err(|error| format!("invalid expected Goal revision: {error}"))?;
    if expected_goal_revision == 0 {
        return Err("expected Goal revision must be positive".to_string());
    }
    let requirement_id = required_arg(args, 1, REQUIREMENT_SKIP_USAGE)?.to_string();
    let mut cursor = 2_usize;
    let requested_run_id = match args.get(cursor).copied() {
        Some("--reason") => None,
        Some(run_id) if !run_id.starts_with("--") => {
            cursor = cursor.saturating_add(1);
            Some(run_id.to_string())
        }
        _ => return Err(format!("Usage: {REQUIREMENT_SKIP_USAGE}")),
    };
    if args.get(cursor).copied() != Some("--reason") {
        return Err(format!("Usage: {REQUIREMENT_SKIP_USAGE}"));
    }
    let reason = args
        .get(cursor.saturating_add(1)..)
        .unwrap_or_default()
        .join(" ");
    if reason.trim().is_empty() {
        return Err(format!("Usage: {REQUIREMENT_SKIP_USAGE}"));
    }
    Ok(ParsedRequirementSkip {
        expected_goal_revision,
        requirement_id,
        requested_run_id,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_current_and_explicit_run_goal_updates() -> Result<(), String> {
        let current = parse_run_goal_update_args(&[
            "2", "--reason", "scope", "changed", "--goal", "finish", "M0",
        ])?;
        assert_eq!(current.expected_goal_revision, 2);
        assert_eq!(current.requested_run_id, None);
        assert_eq!(current.reason, "scope changed");
        assert_eq!(current.new_goal, "finish M0");

        let explicit = parse_run_goal_update_args(&[
            "7",
            "run-1",
            "--reason",
            "user",
            "correction",
            "--goal",
            "new",
            "target",
        ])?;
        assert_eq!(explicit.requested_run_id.as_deref(), Some("run-1"));
        assert_eq!(explicit.reason, "user correction");
        assert_eq!(explicit.new_goal, "new target");
        Ok(())
    }

    #[test]
    fn rejects_incomplete_or_zero_revision_goal_updates() {
        assert!(parse_run_goal_update_args(&[]).is_err());
        assert!(parse_run_goal_update_args(&["0", "--reason", "x", "--goal", "y"]).is_err());
        assert!(parse_run_goal_update_args(&["1", "--reason", "x"]).is_err());
        assert!(parse_run_goal_update_args(&["1", "--reason", "--goal", "y"]).is_err());
    }

    #[test]
    fn parses_requirement_skip_with_optional_run() -> Result<(), String> {
        let current =
            parse_requirement_skip_args(&["2", "req:abc", "--reason", "not", "applicable"])?;
        assert_eq!(current.requested_run_id, None);
        assert_eq!(current.reason, "not applicable");

        let explicit =
            parse_requirement_skip_args(&["3", "req:def", "run-1", "--reason", "user", "waived"])?;
        assert_eq!(explicit.requested_run_id.as_deref(), Some("run-1"));
        assert_eq!(explicit.requirement_id, "req:def");
        Ok(())
    }

    #[test]
    fn parses_exact_subagent_control_identity_and_instruction() -> Result<(), String> {
        let parsed = parse_subagent_control_args(
            &[
                "run-1",
                "task-1",
                "execution-1",
                "3",
                "2",
                "command-1",
                "inspect",
                "the",
                "diff",
            ],
            SUBAGENT_MESSAGE_USAGE,
            true,
        )?;
        assert_eq!(parsed.identity.run_id, "run-1");
        assert_eq!(parsed.identity.plan_revision, 3);
        assert_eq!(parsed.identity.attempt, 2);
        assert_eq!(parsed.instruction.as_deref(), Some("inspect the diff"));
        Ok(())
    }

    #[test]
    fn subagent_interrupt_rejects_instruction_and_zero_attempt() {
        assert!(
            parse_subagent_control_args(
                &["r", "t", "e", "1", "0", "c"],
                SUBAGENT_INTERRUPT_USAGE,
                false,
            )
            .is_err()
        );
        assert!(
            parse_subagent_control_args(
                &["r", "t", "e", "1", "1", "c", "extra"],
                SUBAGENT_INTERRUPT_USAGE,
                false,
            )
            .is_err()
        );
    }
}
