import { beforeEach, describe, expect, it } from 'vitest';
import type { TaskRun } from '../generated';
import { useChatStore } from '../stores/chatStore';
import { useTaskRuntimeStore } from '../stores/taskRuntimeStore';
import { handleChatEvent } from './chatEventHandler';

describe('chat and TaskRuntime lifecycle separation', () => {
  beforeEach(() => {
    useChatStore.setState({ runStatus: 'idle' });
    useTaskRuntimeStore.getState().reset();
  });

  it('does not project a chat terminal status onto the active TaskRun', () => {
    useTaskRuntimeStore.setState({
      activeRun: {
        run_id: 'task-run',
        status: 'running',
      } as TaskRun,
    });
    const assistantIdRef = { current: null as string | null };
    const currentMessageKeyRef = { current: null as string | null };
    const currentMessageIdRef = { current: null as string | null };
    const isCancelledRef = { current: false };
    const currentThinkingIdRef = { current: null as string | null };

    handleChatEvent(
      { type: 'run_status', status: 'completed' },
      {
        assistantIdRef,
        currentMessageKeyRef,
        currentMessageIdRef,
        isCancelledRef,
        currentThinkingIdRef,
      }
    );

    expect(useChatStore.getState().runStatus).toBe('completed');
    expect(useTaskRuntimeStore.getState().activeRun?.status).toBe('running');
  });
});
