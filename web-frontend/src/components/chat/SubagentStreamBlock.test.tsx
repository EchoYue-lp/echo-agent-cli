import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { SubagentRunState } from '../../stores/subagentRunStore';
import { SubagentStreamBlock } from './SubagentStreamBlock';

describe('SubagentStreamBlock', () => {
  it('renders one subagent container with the three requested tabs', () => {
    const run: SubagentRunState = {
      subagentRunId: 'subagent-1',
      runId: 'run-1',
      agent: 'explorer',
      task: '分析项目结构',
      status: 'running',
      startedAt: 1,
      events: [],
    };

    const html = renderToStaticMarkup(
      <SubagentStreamBlock run={run} allRuns={[run]} taskTitle="核证 Agent 实例并发问题" />
    );

    expect(html.match(/role="tablist"/g)).toHaveLength(1);
    expect(html).toContain('子智能体');
    expect(html).toContain('Explore');
    expect(html).toContain('核证 Agent 实例并发问题');
    expect(html).toContain('提示词 / 任务');
    expect(html).toContain('执行细节');
    expect(html).toContain('结果');
  });
});
