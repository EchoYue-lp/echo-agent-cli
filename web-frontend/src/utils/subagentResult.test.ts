import { describe, expect, it } from 'vitest';
import type { SubagentRunState } from '../stores/subagentRunStore';
import { subagentResultPresentation, withoutPromotedThinking } from './subagentResult';

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

  it('promotes the last substantial thinking block when the terminal output only refers above', () => {
    const actualResult = '# Analysis result\n\n' + 'Evidence-backed detail. '.repeat(12);
    const run = completedRun({
      finalOutput: '见上方分析结果。',
      result: {
        contract_version: 0,
        status: 'completed',
        summary: '见上方分析结果。',
        artifacts: [],
        verification: [],
        remaining_work: [],
        touched_files: { read: [], written: [] },
      },
      events: [
        {
          kind: 'subagent',
          subagent_run_id: 'task-1:1',
          run_id: 'run-1',
          task_id: 'task-1',
          agent: 'explorer',
          event: 'thinking_delta',
          content: actualResult,
        },
        {
          kind: 'subagent',
          subagent_run_id: 'task-1:1',
          run_id: 'run-1',
          task_id: 'task-1',
          agent: 'explorer',
          event: 'thinking_ended',
        },
      ],
    });

    expect(subagentResultPresentation(run)).toEqual({
      text: actualResult.trim(),
      promotedThinking: actualResult.trim(),
    });
  });

  it('removes only the promoted result block from execution steps', () => {
    const steps = [
      { type: 'thinking', content: 'initial reasoning' },
      { type: 'tool', content: 'read files' },
      { type: 'thinking', content: 'final report' },
    ];

    expect(withoutPromotedThinking(steps, 'final report')).toEqual(steps.slice(0, 2));
  });
});
