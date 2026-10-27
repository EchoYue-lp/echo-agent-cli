import type { ChatEventEnvelope, HitlRequestIdentity } from '../types/api';
import fixtureText from '../fixtures/chat-event-envelope-v4.json?raw';
import { beforeEach, describe, expect, it } from 'vitest';
import type { TaskRun } from '../generated';
import { useChatStore } from '../stores/chatStore';
import { useTaskRuntimeStore } from '../stores/taskRuntimeStore';
import { handleChatEvent, handleChatEventEnvelope } from './chatEventHandler';

const hitlIdentity = (
  request_id: string,
  kind: HitlRequestIdentity['kind']
): HitlRequestIdentity => ({
  request_id,
  response_target: `target-${request_id}`,
  kind,
  scope: {
    workspace_id: 'global',
    host_generation: 'global',
    conversation_id: 'conversation',
    root_turn_id: 'turn',
    turn_id: 'turn',
  },
});

describe('chat and TaskRuntime lifecycle separation', () => {
  beforeEach(() => {
    useChatStore.setState({ runStatus: 'idle', messages: [], pendingHitlRequests: [] });
    useTaskRuntimeStore.getState().reset();
  });

  it('旧思考起止信号不再把后续正文 Token 改写为 Reasoning', () => {
    const id = useChatStore.getState().startAssistantMessage();
    const ctx = {
      assistantIdRef: { current: id },
      currentMessageKeyRef: { current: null as string | null },
      currentMessageIdRef: { current: null as string | null },
      isCancelledRef: { current: false },
      currentThinkingIdRef: { current: null as string | null },
    };
    handleChatEvent({ type: 'thinking_start' }, ctx);
    handleChatEvent({ type: 'token', data: '已检查当前实现，接下来运行验证。' }, ctx);
    handleChatEvent({ type: 'thinking_end', prompt_tokens: 1, completion_tokens: 1 }, ctx);
    handleChatEvent({ type: 'token', data: '验证完成。' }, ctx);

    const message = useChatStore.getState().messages.find((entry) => entry.id === id);
    expect(message?.content).toBe('已检查当前实现，接下来运行验证。验证完成。');
    expect(message?.thinkingSegments?.some((segment) => segment.content.trim())).toBe(false);
    expect(message?.isStreaming).toBe(true);
  });

  it('原生块独立进入正文时间线，兼容 Token 不重复，最终发布也不结算 TaskRun', () => {
    const id = useChatStore.getState().startAssistantMessage();
    const ctx = {
      assistantIdRef: { current: id },
      currentMessageKeyRef: { current: null as string | null },
      currentMessageIdRef: { current: null as string | null },
      isCancelledRef: { current: false },
      currentThinkingIdRef: { current: null as string | null },
    };
    const base = (JSON.parse(fixtureText) as ChatEventEnvelope[]).at(0);
    if (!base) throw new Error('missing envelope fixture');
    if (base.payload.source !== 'agent') throw new Error('missing agent fixture');
    handleChatEventEnvelope(
      {
        ...base,
        payload: {
          source: 'agent',
          event: {
            ...base.payload.event,
            schema_version: 5,
            payload: {
              type: 'assistant_message',
              data: {
                block: {
                  block_id: 'body',
                  request_id: 'request',
                  sequence: 0,
                  phase: 'commentary',
                  delta: '真实进度',
                  complete: true,
                  provider: null,
                  reasoning: null,
                },
              },
            },
          },
        },
      },
      ctx
    );
    handleChatEvent({ type: 'token', data: '真实进度' }, ctx);
    expect(useChatStore.getState().messages.at(-1)?.assistantBlocks?.at(0)?.content).toBe(
      '真实进度'
    );
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('');
    useTaskRuntimeStore.setState({ activeRun: { run_id: 'task', status: 'running' } as TaskRun });
    handleChatEvent({ type: 'final_answer', data: '最终报告' }, ctx);
    expect(useChatStore.getState().messages.at(-1)?.finalAnswerConfirmed).toBe(true);
    expect(useChatStore.getState().isStreaming).toBe(true);
    expect(useTaskRuntimeStore.getState().activeRun?.status).toBe('running');
  });

  it('无关lifecycle不刷新操作，重放只使用原事件时间，连续模型请求独立计时', () => {
    const id = useChatStore.getState().startAssistantMessage();
    const ctx = {
      assistantIdRef: { current: id },
      currentMessageKeyRef: { current: 'current-root' },
      currentMessageIdRef: { current: id },
      isCancelledRef: { current: false },
      currentThinkingIdRef: { current: null as string | null },
    };
    const base = (JSON.parse(fixtureText) as ChatEventEnvelope[]).at(0);
    if (!base || base.payload.source !== 'agent') throw new Error('missing agent fixture');
    const started: ChatEventEnvelope = {
      ...base,
      root_turn_id: 'current-root',
      event_id: 'model-one',
      timestamp: new Date(1000).toISOString(),
      payload: {
        source: 'agent',
        event: { ...base.payload.event, payload: { type: 'think_start' } },
      },
    };
    handleChatEventEnvelope(started, ctx);
    expect(useChatStore.getState().activity?.lastEventAt).toBe(1000);
    const lifecycle = {
      ...base,
      workspace_id: 'other',
      root_turn_id: 'other',
      timestamp: new Date(9000).toISOString(),
      payload: { source: 'input_lifecycle', event: {} },
    } as ChatEventEnvelope;
    handleChatEventEnvelope(lifecycle, ctx);
    expect(useChatStore.getState().activity?.lastEventAt).toBe(1000);
    handleChatEventEnvelope(
      { ...started, event_id: 'model-two', timestamp: new Date(5000).toISOString() },
      ctx
    );
    expect(useChatStore.getState().activity?.startedAt).toBe(5000);
    expect(useChatStore.getState().activity?.lastEventAt).toBe(5000);
  });

  it('原生请求无需ThinkStart也独立计时，execution settled仍不结束Turn或TaskRun', () => {
    const id = useChatStore.getState().startAssistantMessage();
    const ctx = {
      assistantIdRef: { current: id },
      currentMessageKeyRef: { current: 'native-root' },
      currentMessageIdRef: { current: id },
      isCancelledRef: { current: false },
      currentThinkingIdRef: { current: null as string | null },
    };
    const base = (JSON.parse(fixtureText) as ChatEventEnvelope[]).at(0);
    if (!base || base.payload.source !== 'agent') throw new Error('missing agent fixture');
    const agent = base.payload.event;
    const deliver = (payload: typeof agent.payload, at: number) =>
      handleChatEventEnvelope(
        {
          ...base,
          root_turn_id: 'native-root',
          timestamp: new Date(at).toISOString(),
          payload: { source: 'agent', event: { ...agent, payload } },
        },
        ctx
      );
    deliver(
      {
        type: 'execution_status',
        data: {
          execution_id: 'execution',
          request_id: 'one',
          sequence: 0,
          phase: 'Thinking',
          status: 'waiting',
          reason: null,
        },
      },
      1000
    );
    deliver(
      {
        type: 'assistant_message',
        data: {
          block: {
            block_id: 'body',
            request_id: 'one',
            sequence: 1,
            phase: 'commentary',
            delta: '第一请求',
            complete: true,
            provider: null,
            reasoning: null,
          },
        },
      },
      2000
    );
    expect(useChatStore.getState().activity?.startedAt).toBe(1000);
    deliver(
      {
        type: 'assistant_message',
        data: {
          block: {
            block_id: 'body',
            request_id: 'two',
            sequence: 0,
            phase: 'commentary',
            delta: '第二请求',
            complete: false,
            provider: null,
            reasoning: null,
          },
        },
      },
      5000
    );
    expect(useChatStore.getState().activity?.key).toBe('request:two');
    expect(useChatStore.getState().activity?.startedAt).toBe(5000);
    deliver(
      {
        type: 'assistant_message',
        data: {
          block: {
            block_id: 'body',
            request_id: 'two',
            sequence: 1,
            phase: 'commentary',
            delta: '追加',
            complete: true,
            provider: null,
            reasoning: null,
          },
        },
      },
      5500
    );
    expect(useChatStore.getState().activity?.startedAt).toBe(5000);
    useTaskRuntimeStore.setState({ activeRun: { run_id: 'task', status: 'running' } as TaskRun });
    deliver(
      {
        type: 'execution_status',
        data: {
          execution_id: 'execution',
          request_id: 'two',
          sequence: 2,
          phase: 'Terminal',
          status: 'settled',
          reason: null,
        },
      },
      6000
    );
    expect(useChatStore.getState().isStreaming).toBe(true);
    expect(useChatStore.getState().messages.at(-1)?.finalAnswerConfirmed).toBe(false);
    expect(useTaskRuntimeStore.getState().activeRun?.status).toBe('running');
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

  it('内部执行路径保留在诊断记录中，不写入普通聊天消息', () => {
    const context = {
      assistantIdRef: { current: null as string | null },
      currentMessageKeyRef: { current: null as string | null },
      currentMessageIdRef: { current: null as string | null },
      isCancelledRef: { current: false },
      currentThinkingIdRef: { current: null as string | null },
    };
    useChatStore.getState().setRunStatus('running');
    handleChatEvent({ type: 'execution_path', observed_path: 'formal_plan' }, context);
    expect(useChatStore.getState().messages).toEqual([]);
    expect(useChatStore.getState().runStatus).toBe('running');
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

  it('keeps partial output active until turn_status settles an error event', () => {
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

    expect(useChatStore.getState().runStatus).toBe('running');
    expect(useChatStore.getState().isStreaming).toBe(true);
    expect(useChatStore.getState().messages.at(-1)?.content).toContain('partial answer');
    expect(useChatStore.getState().messages.at(-1)?.content).toContain(
      '[Error] provider disconnected'
    );
    expect(useChatStore.getState().messages.at(-1)?.isStreaming).toBe(true);

    handleChatEvent({ type: 'run_status', status: 'failed' }, context);
    expect(useChatStore.getState().runStatus).toBe('failed');
    expect(useChatStore.getState().isStreaming).toBe(false);
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
        identity: hitlIdentity('approval-1', 'approval'),
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
        identity: hitlIdentity('input-2', 'input'),
        prompt: 'Describe the second change',
      },
      context
    );
    handleChatEvent(
      {
        type: 'approval_request',
        request_id: 'approval-1',
        identity: hitlIdentity('approval-1', 'approval'),
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

    useChatStore.getState().removeHitlRequest(hitlIdentity('input-2', 'input'));
    expect(useChatStore.getState().pendingHitlRequests).toEqual([
      expect.objectContaining({ kind: 'approval', requestId: 'approval-1' }),
    ]);
    expect(useChatStore.getState().runStatus).toBe('waiting_approval');

    useChatStore.getState().removeHitlRequest(hitlIdentity('approval-1', 'approval'));
    expect(useChatStore.getState().pendingHitlRequests).toEqual([]);
    expect(useChatStore.getState().runStatus).toBe('running');
  });

  it('clears every projected HITL request only on typed turn_status', () => {
    useChatStore.getState().enqueueHitlRequest({
      kind: 'input',
      requestId: 'input-terminal',
      identity: hitlIdentity('input-terminal', 'input'),
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

    expect(useChatStore.getState().pendingHitlRequests).toHaveLength(1);
    expect(useChatStore.getState().runStatus).toBe('waiting_input');

    handleChatEvent({ type: 'run_status', status: 'cancelled' }, context);
    expect(useChatStore.getState().pendingHitlRequests).toEqual([]);
    expect(useChatStore.getState().runStatus).toBe('cancelled');
  });
});
