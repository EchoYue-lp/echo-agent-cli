import { describe, expect, it } from 'vitest';
import type { ChatMessage, ToolExecution } from '../../types/api';
import { flattenSteps } from './MessageBubble';

function planCreateTool(): ToolExecution {
  return {
    id: 'call-plan',
    name: 'plan_create',
    args: { title: 'Core 库模块架构分析', description: 'Long description' },
    result: 'created',
    success: true,
    status: 'succeeded',
    stdout: '',
    stderr: '',
    log: '',
    startedAt: 1,
    finishedAt: 2,
  };
}

describe('MessageBubble execution projection', () => {
  it('keeps a final plan_create step visible when executionRounds is incomplete', () => {
    const message: ChatMessage = {
      id: 'assistant-1',
      role: 'assistant',
      content: '',
      timestamp: 1,
      isStreaming: false,
      thinkingSegments: [{ content: 'Create a reviewable plan' }],
      toolCalls: [planCreateTool()],
      executionSteps: [
        { type: 'thinking', index: 0 },
        { type: 'tool', callId: 'call-plan' },
      ],
      executionRounds: [{ thinking: { content: 'Create a reviewable plan' }, toolCallIds: [] }],
    };

    const projection = flattenSteps(message);
    expect(projection.steps).toHaveLength(2);
    expect(projection.steps[1]?.toolCall?.name).toBe('plan_create');
  });
});
