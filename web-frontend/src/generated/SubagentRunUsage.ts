// TypeScript wire projection of echo_agent::agent::subagent::SubagentUsage.

/**
 * Aggregate framework usage for a single Subagent run.
 *
 * All fields are `Option` because they are populated progressively: a run
 * that just started has no usage yet. `duration_ms` is finalized on
 * completion.
 */
export type SubagentRunUsage = {
  /**
   * Total wall-clock duration in milliseconds (None while running).
   */
  duration_ms: bigint | null;
  /**
   * Total tokens consumed (input + output), if reported by the framework.
   */
  tokens_used: bigint | null;
  /**
   * Number of ReAct iterations executed, if reported.
   */
  iterations: bigint | null;
};
