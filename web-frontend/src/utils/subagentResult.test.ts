import { describe, expect, it } from 'vitest';
import type { SubagentRunState } from '../stores/subagentRunStore';
import { subagentResultPresentation } from './subagentResult';

function completedRun(overrides: Partial<SubagentRunState> = {}): SubagentRunState {
  return {
    subagentRunId: 'task-1:1',
    runId: 'run-1',
    taskId: 'task-1',
    agent: 'explorer',
    status: 'completed',
    startedAt: 1,
    events: [],
    result: {
      contract_version: 1,
      status: 'completed',
      summary: 'self-contained summary',
      artifacts: [],
      verification: [],
      remaining_work: [],
      touched_files: { read: [], written: [] },
    },
    ...overrides,
  };
}

describe('subagentResultPresentation', () => {
  it('uses the complete terminal output and removes the internal result contract', () => {
    const run = completedRun({
      finalOutput:
        '# Architecture analysis\n\nComplete report.\n\n## Result\n```json\n{"contract_version":1,"summary":"done"}\n```',
    });

    expect(subagentResultPresentation(run)).toEqual({
      text: '# Architecture analysis\n\nComplete report.',
    });
  });

  it('keeps a user-facing Result section when it is not the internal JSON contract', () => {
    const finalOutput = '# Analysis\n\n## Result\n\nThe measured result is 42.';

    expect(subagentResultPresentation(completedRun({ finalOutput }))).toEqual({
      text: finalOutput,
    });
  });

  it('falls back to the terminal summary when no full output is available', () => {
    expect(subagentResultPresentation(completedRun())).toEqual({ text: 'self-contained summary' });
  });
});
