import { describe, expect, it } from 'vitest';
import type { ToolExecution } from '../../types/api';
import { toolExecutionGroupPresentation } from './ToolExecutionGroup';

describe('ToolExecutionGroup', () => {
  it('hides individual tool calls by default', () => {
    const tools: ToolExecution[] = ['call-a', 'call-b'].map((id, index) => ({
      id,
      call_id: id,
      owner: { kind: 'chat', message_id: 'assistant-1' },
      name: `tool_${index + 1}`,
      args_preview: '',
      status: 'succeeded',
      started_at: 1,
      finished_at: 2,
      duration_ms: 1,
      detail_ref: id,
    }));
    const presentation = toolExecutionGroupPresentation(
      ['call-a', 'call-b'],
      Object.fromEntries(tools.map((tool) => [tool.id, tool]))
    );

    expect(presentation).toMatchObject({
      label: '已执行 2 个工具',
      runningCount: 0,
      failedCount: 0,
      missingCount: 0,
    });
  });

  it('does not report success when referenced tool state is missing', () => {
    expect(toolExecutionGroupPresentation(['call-a', 'call-b'], {})).toMatchObject({
      label: '2 个工具 · 2 个状态未恢复',
      missingCount: 2,
    });
  });
});
