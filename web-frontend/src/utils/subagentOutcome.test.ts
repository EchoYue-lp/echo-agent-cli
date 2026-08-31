import { describe, expect, it } from 'vitest';
import type { SubagentRunState } from '../stores/subagentRunStore';
import { subagentOutcomePresentation } from './subagentOutcome';

function completedRun(overrides: Partial<SubagentRunState> = {}): SubagentRunState {
  return {
    subagentRunId: 'task-1:1',
    runId: 'run-1',
    taskId: 'task-1',
    agent: 'explorer',
    status: 'completed',
    startedAt: 1,
    events: [],
    outcome: {
      contract_version: 1,
      status: 'completed',
      summary: 'self-contained summary',
      artifacts: [],
      evidence: [],
      verification: [],
      remaining_work: [],
      touched_files: { read: [], written: [] },
    },
    ...overrides,
  };
}

describe('subagentOutcomePresentation', () => {
  it('uses the complete terminal output and removes the internal result contract', () => {
    const run = completedRun({
      finalOutput:
        '# Architecture analysis\n\nComplete report.\n\n## Result\n```json\n{"contract_version":1,"summary":"done"}\n```',
    });

    expect(subagentOutcomePresentation(run)).toEqual({
      text: '# Architecture analysis\n\nComplete report.',
    });
  });

  it('keeps a user-facing Result section when it is not the internal JSON contract', () => {
    const finalOutput = '# Analysis\n\n## Result\n\nThe measured result is 42.';

    expect(subagentOutcomePresentation(completedRun({ finalOutput }))).toEqual({
      text: finalOutput,
    });
  });

  it('falls back to the terminal summary when no full output is available', () => {
    expect(subagentOutcomePresentation(completedRun())).toEqual({ text: 'self-contained summary' });
  });
});
