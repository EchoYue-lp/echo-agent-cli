pub const TASK_GOAL_USAGE: &str =
    "/task-goal <expected-revision> [run-id] --reason <reason> --goal <new-goal>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRunGoalUpdate {
    pub expected_goal_revision: u64,
    pub requested_run_id: Option<String>,
    pub reason: String,
    pub new_goal: String,
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
}
