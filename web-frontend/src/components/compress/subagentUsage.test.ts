import { describe, expect, it } from 'vitest';
import { isCanonicalUsageEvent, summarizeSubagentUsage } from './subagentUsage';

const canonical = {
  event: 'usage',
  model: 'test-model',
  prompt_tokens: 100,
  completion_tokens: 20,
  total_tokens: 120,
  cached_prompt_tokens: 40,
  cache_creation_prompt_tokens: 10,
  usage_reported: true,
  usage_event_id: 'task-1:usage:1',
};

describe('subagent usage', () => {
  it('rejects thinking-ended compatibility events', () => {
    expect(
      isCanonicalUsageEvent({
        event: 'thinking_ended',
        prompt_tokens: 100,
        completion_tokens: 20,
      })
    ).toBe(false);
  });

  it('deduplicates canonical events by usage event id', () => {
    const summary = summarizeSubagentUsage(
      [
        {
          conversationId: 'conv-a',
          status: 'completed',
          usageEvents: [canonical, { ...canonical }],
        },
      ],
      'conv-a'
    );

    expect(summary.calls).toBe(1);
    expect(summary.input).toBe(100);
    expect(summary.output).toBe(20);
  });

  it('excludes runs from other conversations', () => {
    const summary = summarizeSubagentUsage(
      [
        { conversationId: 'conv-a', status: 'running', usageEvents: [canonical] },
        {
          conversationId: 'conv-b',
          status: 'completed',
          usageEvents: [{ ...canonical, usage_event_id: 'task-2:usage:1' }],
        },
      ],
      'conv-a'
    );

    expect(summary.total).toBe(1);
    expect(summary.running).toBe(1);
    expect(summary.calls).toBe(1);
  });

  it('counts provider usage reports that are explicitly unavailable', () => {
    const summary = summarizeSubagentUsage(
      [
        {
          conversationId: 'conv-a',
          status: 'completed',
          usageEvents: [
            {
              ...canonical,
              usage_reported: false,
              usage_event_id: 'task-1:usage:missing',
            },
          ],
        },
      ],
      'conv-a'
    );

    expect(summary.calls).toBe(1);
    expect(summary.missingUsage).toBe(1);
    expect(summary.input).toBe(0);
    expect(summary.output).toBe(0);
  });
});
