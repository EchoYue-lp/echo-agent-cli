import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { SubagentRunState } from '../../stores/subagentRunStore';
import { SubagentDetailView } from './SubagentDetailView';

describe('SubagentDetailView', () => {
  it('renders exact-attempt controls for a running Subagent', () => {
    const run: SubagentRunState = {
      subagentRunId: 'run-1:task-1:3:2:claim-1',
      runId: 'run-1',
      taskId: 'task-1',
      planRevision: 3,
      attempt: 2,
      agent: 'implementer',
      status: 'running',
      startedAt: 1,
      events: [],
    };

    const html = renderToStaticMarkup(<SubagentDetailView run={run} onBack={() => undefined} />);

    expect(html).toContain('aria-label="Message Subagent"');
    expect(html).toContain('aria-label="Interrupt Subagent"');
    expect(html).toContain('aria-label="Queue guidance for next attempt"');
  });

  it('shows the complete terminal output without protocol metadata', () => {
    const actualResult = '# Complete architecture report\n\n' + 'User-facing details. '.repeat(12);
    const run: SubagentRunState = {
      subagentRunId: 'task-1:1',
      runId: 'run-1',
      taskId: 'task-1',
      agent: 'explorer',
      status: 'completed',
      startedAt: 1,
      finalOutput: `${actualResult}\n\n## Result\n\n\`\`\`json\n{"contract_version":1,"summary":"done"}\n\`\`\``,
      outcome: {
        contract_version: 0,
        status: 'completed',
        summary: '见上方分析结果。',
        artifacts: [],
        evidence: [],
        verification: [],
        remaining_work: [],
        touched_files: { read: ['Cargo.toml'], written: [] },
      },
      events: [],
    };

    const html = renderToStaticMarkup(<SubagentDetailView run={run} onBack={() => undefined} />);

    expect(html).toContain('Complete architecture report');
    expect(html).toContain('User-facing details');
    expect(html).not.toContain('见上方分析结果');
    expect(html).not.toContain('## Result');
    expect(html).not.toContain('Cargo.toml');
  });
});
