// Framework-owned SubagentStatus contract exposed by the EKO IPC surface.

/**
 * Lifecycle status of a [`SubagentRun`]. Mirrors the coarse states the
 * frontend already renders for the unified subagent concept, minus the
 * pending state (a SubagentRun only exists once dispatch has started).
 */
export type SubagentStatus = 'running' | 'completed' | 'failed' | 'cancelled' | 'timed_out';
