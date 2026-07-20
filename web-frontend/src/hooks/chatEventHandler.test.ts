import { beforeEach, describe, expect, it } from 'vitest';
import type { TaskRun } from '../generated';
import { useChatStore } from '../stores/chatStore';
import { useTaskRuntimeStore } from '../stores/taskRuntimeStore';
import { handleChatEvent } from './chatEventHandler';

describe('chat and TaskRuntime lifecycle separation', () => {
  beforeEach(() => {
    useChatStore.setState({ runStatus: 'idle', messages: [] });
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

  it('renders previously dropped safety and guard notices', () => {
    handleChatEvent(
      {
        type: 'notice',
        level: 'warning',
        code: 'guard_triggered',
        message: 'Guard workspace_scope triggered (blocked=true)',
      },
      {
        assistantIdRef: { current: null },
        currentMessageKeyRef: { current: null },
        currentMessageIdRef: { current: null },
        isCancelledRef: { current: false },
        currentThinkingIdRef: { current: null },
      }
    );

    expect(useChatStore.getState().messages.at(-1)?.content).toContain('workspace_scope');
  });

  it('only surfaces execution path when observed behavior differs', () => {
    const context = {
      assistantIdRef: { current: null as string | null },
      currentMessageKeyRef: { current: null as string | null },
      currentMessageIdRef: { current: null as string | null },
      isCancelledRef: { current: false },
      currentThinkingIdRef: { current: null as string | null },
    };
    handleChatEvent(
      { type: 'execution_path', requested_mode: 'auto', observed_path: 'auto' },
      context
    );
    expect(useChatStore.getState().messages).toHaveLength(0);

    handleChatEvent(
      { type: 'execution_path', requested_mode: 'task', observed_path: 'chat' },
      context
    );
    expect(useChatStore.getState().messages.at(-1)?.content).toContain('task -> chat');
  });

  it('projects reported LLM usage into the context indicator state', () => {
    handleChatEvent(
      {
        type: 'llm_usage',
        model: 'deepseek-v4-flash',
        prompt_tokens: 32_000,
        completion_tokens: 800,
        total_tokens: 32_800,
        cached_prompt_tokens: 28_000,
        cache_creation_prompt_tokens: 0,
        usage_reported: true,
      },
      {
        assistantIdRef: { current: null },
        currentMessageKeyRef: { current: null },
        currentMessageIdRef: { current: null },
        isCancelledRef: { current: false },
        currentThinkingIdRef: { current: null },
      }
    );

    expect(useChatStore.getState().contextWindow).toMatchObject({
      inputTokens: 32_000,
      cachedTokens: 28_000,
      usageReported: true,
    });
    expect(useChatStore.getState().usageAccumulator).toEqual({
      totalInput: 32_000,
      totalCached: 28_000,
    });
  });
});
