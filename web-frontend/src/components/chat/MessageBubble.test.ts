import { describe, expect, it } from 'vitest';
import type { ChatMessage } from '../../types/api';
import { flattenSteps, groupExecutionSteps, isExecutionProcessCompleted } from './MessageBubble';

describe('MessageBubble execution projection', () => {
  it('uses the chronological step stream when executionRounds is incomplete', () => {
    const message: ChatMessage = {
      id: 'assistant-1',
      role: 'assistant',
      content: '',
      timestamp: 1,
      isStreaming: false,
      thinkingSegments: [
        { content: 'Create a reviewable plan' },
        { content: 'Check the created plan' },
      ],
      executionSteps: [
        { type: 'thinking', index: 0 },
        { type: 'tool', callId: 'call-plan' },
        { type: 'thinking', index: 1 },
      ],
      executionRounds: [{ thinking: { content: 'Create a reviewable plan' }, toolCallIds: [] }],
    };

    const projection = flattenSteps(message);
    expect(projection.steps).toHaveLength(3);
    expect(projection.steps[1]?.toolId).toBe('call-plan');
    expect(projection.steps[2]?.thinkingContent).toBe('Check the created plan');
  });

  it('groups only consecutive tool calls between thinking segments', () => {
    const message: ChatMessage = {
      id: 'assistant-2',
      role: 'assistant',
      content: '',
      timestamp: 1,
      isStreaming: true,
      thinkingSegments: [{ content: 'Inspect files' }, { content: 'Apply the fix' }],
      executionSteps: [
        { type: 'thinking', index: 0 },
        { type: 'tool', callId: 'call-list' },
        { type: 'tool', callId: 'call-read' },
        { type: 'thinking', index: 1 },
        { type: 'tool', callId: 'call-edit' },
      ],
    };

    const projection = flattenSteps(message);
    expect(groupExecutionSteps(projection.steps)).toEqual([
      { type: 'thinking', content: 'Inspect files' },
      { type: 'tools', toolIds: ['call-list', 'call-read'] },
      { type: 'thinking', content: 'Apply the fix' },
      { type: 'tools', toolIds: ['call-edit'] },
    ]);
  });

  it('falls back to round-level history when chronological steps are unavailable', () => {
    const message: ChatMessage = {
      id: 'assistant-3',
      role: 'assistant',
      content: '',
      timestamp: 1,
      executionRounds: [
        { thinking: { content: 'Inspect the history' }, toolCallIds: ['call-history'] },
      ],
    };

    const projection = flattenSteps(message);
    expect(groupExecutionSteps(projection.steps)).toEqual([
      { type: 'thinking', content: 'Inspect the history' },
      { type: 'tools', toolIds: ['call-history'] },
    ]);
  });

  it('does not collapse reloaded history while its TaskRun is still active', () => {
    expect(isExecutionProcessCompleted(false, true, 'running', ['completed'])).toBe(false);
    expect(isExecutionProcessCompleted(false, true, 'completed', ['completed'])).toBe(true);
    expect(isExecutionProcessCompleted(false, true, 'completed', ['running'])).toBe(true);
  });

  it('waits for every visible Subagent before collapsing the process', () => {
    expect(isExecutionProcessCompleted(false, false, null, ['completed', 'running'])).toBe(false);
    expect(isExecutionProcessCompleted(false, false, null, ['completed', 'failed'])).toBe(true);
  });
});
