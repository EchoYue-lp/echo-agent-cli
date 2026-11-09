import { describe, expect, it } from 'vitest';
import type { ChatMessage } from '../../types/api';
import { flattenSteps, groupExecutionSteps, isExecutionProcessCompleted } from './MessageBubble';

describe('MessageBubble execution projection', () => {
  it('keeps canonical tool reference order without round history', () => {
    const message: ChatMessage = {
      id: 'assistant-1',
      role: 'assistant',
      content: '',
      timestamp: 1,
      executionSteps: [
        { type: 'tool', callId: 'call-list' },
        { type: 'tool', callId: 'call-read' },
      ],
    };
    const projection = flattenSteps(message);
    expect(projection.steps.map((step) => step.toolId)).toEqual(['call-list', 'call-read']);
    expect(groupExecutionSteps(projection.steps)).toEqual([
      { type: 'tools', toolIds: ['call-list', 'call-read'] },
    ]);
  });

  it('does not fabricate process steps without native/tool facts', () => {
    const message: ChatMessage = {
      id: 'empty',
      role: 'assistant',
      content: 'ordinary',
      timestamp: 1,
    };
    expect(flattenSteps(message).steps).toEqual([]);
    expect(groupExecutionSteps([])).toEqual([]);
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
