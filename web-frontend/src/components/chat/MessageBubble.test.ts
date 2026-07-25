import { describe, expect, it } from 'vitest';
import type { ChatMessage } from '../../types/api';
import { flattenSteps } from './MessageBubble';

describe('MessageBubble execution projection', () => {
  it('keeps a final plan_create step visible when executionRounds is incomplete', () => {
    const message: ChatMessage = {
      id: 'assistant-1',
      role: 'assistant',
      content: '',
      timestamp: 1,
      isStreaming: false,
      thinkingSegments: [{ content: 'Create a reviewable plan' }],
      executionSteps: [
        { type: 'thinking', index: 0 },
        { type: 'tool', callId: 'call-plan' },
      ],
      executionRounds: [{ thinking: { content: 'Create a reviewable plan' }, toolCallIds: [] }],
    };

    const projection = flattenSteps(message);
    expect(projection.steps).toHaveLength(2);
    expect(projection.steps[1]?.toolId).toBe('call-plan');
  });
});
