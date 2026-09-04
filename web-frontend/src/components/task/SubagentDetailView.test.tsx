import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { SubagentRunState } from '../../stores/subagentRunStore';
import { SubagentDetailView } from './SubagentDetailView';

describe('SubagentDetailView', () => {
  it('renders an inline exact-attempt composer for a running Subagent', () => {
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

    expect(html).toContain('aria-label="Subagent 消息"');
    expect(html).toContain('aria-label="发送 Subagent 消息"');
    expect(html).toContain('aria-label="中断 Subagent"');
    expect(html).toContain('attempt 2');
    expect(html).not.toContain('window.prompt');
  });

  it('shows task, execution and result as one stream without tabs', () => {
    const run: SubagentRunState = {
      subagentRunId: 'task-1:1',
      runId: 'run-1',
      taskId: 'task-1',
      agent: 'explorer',
      task: '重构支付模块并补齐退款边界单测',
      status: 'running',
      startedAt: 1,
      events: [],
    };

    const html = renderToStaticMarkup(<SubagentDetailView run={run} onBack={() => undefined} />);

    // Task prompt renders inline (main-chat user-bubble style), no tab shell.
    expect(html).toContain('重构支付模块并补齐退款边界单测');
    expect(html).not.toContain('提示词 / 任务');
    expect(html).not.toContain('aria-pressed');
  });

  it('shows the complete terminal output without protocol metadata', () => {
    const actualResult = '# Complete architecture report\n\n' + 'User-facing details. '.repeat(12);
    const run: SubagentRunState = {
      subagentRunId: 'task-1:1',
      runId: 'run-1',
      taskId: 'task-1',
      agent: 'explorer',
      task: '重构支付模块并补齐退款边界单测',
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
    // The task prompt stays visible alongside the terminal output.
    expect(html).toContain('重构支付模块并补齐退款边界单测');
    expect(html).not.toContain('见上方分析结果');
    expect(html).not.toContain('## Result');
    expect(html).not.toContain('Cargo.toml');
  });

  it('routes a settled attempt through the follow-up composer presentation', () => {
    const run: SubagentRunState = {
      subagentRunId: 'run-1:task-1:3:2:claim-1',
      runId: 'run-1',
      workspaceId: 'workspace-1',
      taskId: 'task-1',
      planRevision: 3,
      attempt: 2,
      agent: 'reviewer',
      status: 'failed',
      startedAt: 1,
      error: 'provider unavailable',
      outcome: {
        contract_version: 1,
        status: 'failed',
        summary: 'provider unavailable',
        artifacts: [],
        evidence: [],
        verification: [],
        remaining_work: ['provider unavailable'],
        touched_files: { read: [], written: [] },
      },
      events: [],
    };

    const html = renderToStaticMarkup(<SubagentDetailView run={run} onBack={() => undefined} />);

    expect(html).toContain('aria-label="Subagent 后续任务"');
    expect(html).toContain('aria-label="发送 Subagent 后续任务"');
    expect(html.match(/provider unavailable/g)).toHaveLength(1);
    expect(html).not.toContain('未完成');
  });
});
