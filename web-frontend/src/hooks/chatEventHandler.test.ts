import { beforeEach, describe, expect, it } from 'vitest';
import type { TaskRun } from '../generated';
import { useChatStore } from '../stores/chatStore';
import { useTaskRuntimeStore } from '../stores/taskRuntimeStore';
import { handleChatEvent } from './chatEventHandler';

describe('chat and TaskRuntime lifecycle separation', () => {
  beforeEach(() => {
    useChatStore.setState({ runStatus: 'idle', messages: [], pendingHitlRequests: [] });
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

  it('keeps partial output and the failed terminal state on an error event', () => {
    const assistantId = useChatStore.getState().startAssistantMessage('assistant-1');
    useChatStore.getState().appendToken(assistantId, 'partial answer');
    const context = {
      assistantIdRef: { current: assistantId as string | null },
      currentMessageKeyRef: { current: 'message-key' as string | null },
      currentMessageIdRef: { current: 'message-id' as string | null },
      isCancelledRef: { current: false },
      currentThinkingIdRef: { current: null as string | null },
    };

    handleChatEvent({ type: 'error', message: 'provider disconnected' }, context);

    const state = useChatStore.getState();
    expect(state.runStatus).toBe('failed');
    expect(state.messages.at(-1)?.content).toContain('partial answer');
    expect(state.messages.at(-1)?.content).toContain('[Error] provider disconnected');
    expect(state.messages.at(-1)?.isStreaming).toBe(false);
  });

  it('queues concurrent HITL requests and removes only the exact request id', () => {
    const context = {
      assistantIdRef: { current: null as string | null },
      currentMessageKeyRef: { current: 'turn' as string | null },
      currentMessageIdRef: { current: 'message' as string | null },
      isCancelledRef: { current: false },
      currentThinkingIdRef: { current: null as string | null },
    };

    handleChatEvent(
      {
        type: 'approval_request',
        request_id: 'approval-1',
        tool_name: 'write_file',
        args: { path: 'first.txt' },
        prompt: 'Approve the first write',
      },
      context
    );
    handleChatEvent(
      {
        type: 'input_request',
        request_id: 'input-2',
        prompt: 'Describe the second change',
      },
      context
    );
    handleChatEvent(
      {
        type: 'approval_request',
        request_id: 'approval-1',
        tool_name: 'write_file',
        args: { path: 'duplicate.txt' },
        prompt: 'Duplicate delivery',
      },
      context
    );

    expect(useChatStore.getState().pendingHitlRequests).toEqual([
      expect.objectContaining({ kind: 'approval', requestId: 'approval-1' }),
      expect.objectContaining({ kind: 'input', requestId: 'input-2' }),
    ]);
    expect(useChatStore.getState().runStatus).toBe('waiting_approval');

    useChatStore.getState().removeHitlRequest('input-2');
    expect(useChatStore.getState().pendingHitlRequests).toEqual([
      expect.objectContaining({ kind: 'approval', requestId: 'approval-1' }),
    ]);
    expect(useChatStore.getState().runStatus).toBe('waiting_approval');

    useChatStore.getState().removeHitlRequest('approval-1');
    expect(useChatStore.getState().pendingHitlRequests).toEqual([]);
    expect(useChatStore.getState().runStatus).toBe('running');
  });

  it('clears every projected HITL request only on a real terminal event', () => {
    useChatStore.getState().enqueueHitlRequest({
      kind: 'input',
      requestId: 'input-terminal',
      prompt: 'Still pending',
    });
    const context = {
      assistantIdRef: { current: null as string | null },
      currentMessageKeyRef: { current: 'turn' as string | null },
      currentMessageIdRef: { current: 'message' as string | null },
      isCancelledRef: { current: false },
      currentThinkingIdRef: { current: null as string | null },
    };

    handleChatEvent({ type: 'cancelled' }, context);

    expect(useChatStore.getState().pendingHitlRequests).toEqual([]);
    expect(useChatStore.getState().runStatus).toBe('cancelled');
  });
});
