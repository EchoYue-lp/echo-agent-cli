import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { SubagentRunState } from '../../stores/subagentRunStore';
import { SubagentDetailView } from './SubagentDetailView';

describe('SubagentDetailView', () => {
  it('shows the complete terminal output without protocol or process metadata', () => {
    const actualResult = '# Complete architecture report\n\n' + 'User-facing details. '.repeat(12);
    const run: SubagentRunState = {
      subagentRunId: 'task-1:1',
      runId: 'run-1',
      taskId: 'task-1',
      agent: 'explorer',
      status: 'completed',
      startedAt: 1,
      streamedText: 'streamed final text',
      finalOutput: '见上方分析结果。',
      result: {
        contract_version: 0,
        status: 'completed',
        summary: '见上方分析结果。',
        artifacts: [],
        verification: [],
        remaining_work: [],
        touched_files: { read: ['Cargo.toml'], written: [] },
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
    };

    const html = renderToStaticMarkup(
      <SubagentDetailView run={run} allRuns={[run]} onBack={() => undefined} />
    );

    expect(html).toContain('Complete architecture report');
    expect(html).toContain('User-facing details');
    expect(html).not.toContain('见上方分析结果');
    expect(html).not.toContain('streamed final text');
    expect(html).not.toContain('## Result');
    expect(html).not.toContain('Cargo.toml');
  });
});
