// TypeScript wire projection of echo_agent::agent::subagent::SubagentCommandPhase.

/**
 * Product projection of the framework tracked receipt for one exact
 * Subagent command. The framework remains the live authority; this value is
 * only the durable app boundary exposed to surfaces and recovery.
 */
export type SubagentControlPhase = 'persisted' | 'mailbox_accepted' | 'drained' | 'turn_settled';
