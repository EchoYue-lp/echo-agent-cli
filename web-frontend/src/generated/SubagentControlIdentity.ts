// TypeScript wire projection of echo_agent::agent::subagent::SubagentCommandIdentity.

/**
 * Durable identity for one user control command and one exact task attempt.
 */
export type SubagentControlIdentity = {
  run_id: string;
  task_id: string;
  execution_id: string;
  plan_revision: number;
  attempt: number;
  command_id: string;
};
