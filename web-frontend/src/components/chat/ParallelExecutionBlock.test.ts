import { describe, expect, it } from 'vitest';
import type { TaskRun } from '../../generated';
import type { SubagentRunState } from '../../stores/subagentRunStore';
import { visibleSubagentRuns } from './ParallelExecutionBlock';

function run(overrides: Partial<SubagentRunState> = {}): SubagentRunState {
  return {
    subagentRunId: 'agent_tool-1',
    runId: '',
    agent: 'explorer',
    status: 'running',
    startedAt: 1,
    events: [],
    ...overrides,
  };
}

describe('ParallelExecutionBlock visibility', () => {
  it('keeps an exact-message Subagent visible despite an unrelated active TaskRun', () => {
    const activeRun = {
      run_id: 'old-formal-run',
      conversation_id: 'conversation-1',
    } as TaskRun;
    const candidate = run({
      messageId: 'assistant-message-2',
      conversationId: 'conversation-1',
    });

    expect(
      visibleSubagentRuns([candidate], activeRun, 'assistant-message-2', 'assistant-message-2')
    ).toEqual([candidate]);
  });

  it('does not move a message-bound Subagent onto a later assistant message', () => {
    const candidate = run({ messageId: 'assistant-message-1' });

    expect(
      visibleSubagentRuns([candidate], null, 'assistant-message-2', 'assistant-message-2')
    ).toEqual([]);
  });

  it('uses the active formal run only for events without a message identity', () => {
    const activeRun = {
      run_id: 'formal-run',
      conversation_id: 'conversation-1',
    } as TaskRun;
    const candidate = run({ runId: 'formal-run', messageId: undefined });

    expect(
      visibleSubagentRuns([candidate], activeRun, 'assistant-message', 'assistant-message')
    ).toEqual([candidate]);
  });

  it('shows only the latest retry attempt for one PlanTask', () => {
    const first = run({
      subagentRunId: 'task-1:1',
      runId: 'formal-run',
      taskId: 'task-1',
      messageId: 'assistant-message',
      status: 'failed',
      startedAt: 10,
    });
    const retry = run({
      subagentRunId: 'task-1:2',
      runId: 'formal-run',
      taskId: 'task-1',
      messageId: 'assistant-message',
      startedAt: 20,
    });

    expect(
      visibleSubagentRuns([first, retry], null, 'assistant-message', 'assistant-message')
    ).toEqual([retry]);
  });
});
