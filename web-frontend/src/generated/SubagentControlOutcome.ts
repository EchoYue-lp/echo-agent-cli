// TypeScript wire projection of echo_agent::agent::AgentSteerTurnOutcome.

/**
 * Terminal outcome of the owning framework turn, when one is available.
 */
export type SubagentControlOutcome = 'completed' | 'failed' | 'cancelled' | 'dropped';
