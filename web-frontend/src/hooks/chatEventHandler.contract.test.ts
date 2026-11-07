import { beforeEach, describe, expect, it, vi } from 'vitest';
import fixtureText from '../fixtures/chat-event-envelope-v4.json?raw';
import { useChatStore } from '../stores/chatStore';
import { subagentRunStoreKey, useSubagentRunStore } from '../stores/subagentRunStore';
import type {
  ChatEventEnvelope,
  ConversationInputFact,
  TranscriptProjectionSettlement,
} from '../types/api';
import { handleChatEvent, handleChatEventEnvelope } from './chatEventHandler';
import { resetChatEventCursorsForTest } from './chatEventSequencer';

const fixture = JSON.parse(fixtureText) as ChatEventEnvelope[];

const context = (): Parameters<typeof handleChatEventEnvelope>[1] => ({
  assistantIdRef: { current: null },
  currentMessageKeyRef: { current: null },
  currentMessageIdRef: { current: null },
  isCancelledRef: { current: false },
  currentThinkingIdRef: { current: null },
});

function savingEvent(
  sequence: number,
  status: TranscriptProjectionSettlement['status'],
  operationId: string | null,
  timestamp = sequence * 1000
): ChatEventEnvelope {
  const base = fixture.at(0);
  if (!base || base.payload.source !== 'agent') throw new Error('agent fixture missing');
  return {
    ...base,
    event_id: `save-outer-${sequence}`,
    sequence,
    timestamp: new Date(timestamp).toISOString(),
    payload: {
      source: 'agent',
      event: {
        ...base.payload.event,
        event_id: `save-native-${sequence}`,
        schema_version: 5,
        payload: {
          type: 'transcript_projection_settlement',
          data: {
            status,
            operation_id: operationId,
            conversation_id: base.conversation_id,
            generation_id: 'original-generation',
            attempt: operationId ? 1 : 0,
            error_class: status === 'settled' ? null : 'outcome_unknown',
            detail: status === 'settled' ? null : '原保存结果尚未确认',
          },
        },
      },
    },
  };
}

describe('canonical chat event v4 contract', () => {
  beforeEach(() => {
    useChatStore.getState().clearMessages();
    useSubagentRunStore.getState().clear();
    resetChatEventCursorsForTest();
  });

  it('保留中断前的已发布发现，并把迟到的原保存事实按源身份显示一次', () => {
    const base = fixture.at(0);
    if (!base || base.payload.source !== 'agent') throw new Error('agent fixture missing');
    const refs = context();
    const assistantId = useChatStore.getState().startAssistantMessage(base.root_turn_id);
    refs.assistantIdRef.current = assistantId;
    useChatStore.getState().appendAssistantBlock(
      assistantId,
      {
        block_id: 'confirmed-finding',
        request_id: 'confirmed-request',
        sequence: 1,
        phase: 'partial_answer',
        delta: '已经确认的夹具发现',
        complete: true,
        provider: 'fixture',
        reasoning: null,
      },
      'confirmed-execution',
      'confirmed-publication'
    );
    const before = useChatStore
      .getState()
      .messages.find((message) => message.id === assistantId)?.assistantBlocks;
    handleChatEvent({ type: 'error', message: '原执行中断' }, refs);
    expect(refs.isCancelledRef.current).toBe(true);
    const saving = {
      ...base,
      event_id: 'original-save-fact',
      sequence: 7,
      payload: {
        source: 'agent',
        event: {
          ...base.payload.event,
          event_id: 'original-native-save-fact',
          schema_version: 5,
          payload: {
            type: 'transcript_projection_settlement',
            data: {
              status: 'blocked',
              operation_id: 'original-save-operation',
              conversation_id: base.conversation_id,
              generation_id: 'original-generation',
              attempt: 1,
              error_class: 'outcome_unknown',
              detail: '原保存结果尚未确认',
            },
          },
        },
      },
    } as unknown as ChatEventEnvelope;
    handleChatEventEnvelope(saving, refs);
    handleChatEventEnvelope(saving, refs);
    expect(
      useChatStore.getState().messages.find((message) => message.id === assistantId)
        ?.assistantBlocks
    ).toEqual(before);
    expect(
      useChatStore
        .getState()
        .messages.filter((message) => message.content.includes('原保存结果尚未确认'))
    ).toHaveLength(1);
    expect(useChatStore.getState().runStatus).toBe('running');
    handleChatEvent({ type: 'run_status', status: 'failed' }, refs);
    expect(useChatStore.getState().runStatus).toBe('failed');
    expect(
      useChatStore
        .getState()
        .messages.some((message) => message.content.includes('原保存结果尚未确认'))
    ).toBe(true);
  });

  it.each(['settled', 'deferred', 'blocked', 'conflict'] as const)(
    '处理 schema5 会话投影结算 %s，同时保留真正的运行终态',
    (status) => {
      const base = fixture.at(0);
      if (!base) throw new Error('contract fixture is incomplete');
      useChatStore.getState().setRunStatus('running');
      const settlement = savingEvent(1, status, 'transcript-projection-v1:original');
      expect(() => handleChatEventEnvelope(settlement, context())).not.toThrow();
      expect(useChatStore.getState().runStatus).toBe('running');
      expect(useChatStore.getState().isStreaming).toBe(true);
      if (status !== 'settled') {
        expect(useChatStore.getState().messages.at(-1)?.content).toContain('outcome_unknown');
        expect(useChatStore.getState().messages.at(-1)?.content).toContain('原保存结果尚未确认');
      }
      handleChatEventEnvelope(
        { ...base, payload: { source: 'turn_status', event: { status: 'completed' } } },
        context()
      );
      expect(useChatStore.getState().runStatus).toBe('completed');
      expect(useChatStore.getState().isStreaming).toBe(false);
    }
  );

  it('保存观察只采用真实batch确认，冷重放可恢复且不替其它操作清除未知结果', () => {
    const events = [
      savingEvent(1, 'settled', 'saved-before'),
      savingEvent(2, 'blocked', 'unknown-original'),
      savingEvent(3, 'settled', null),
      savingEvent(4, 'settled', 'different-operation'),
      savingEvent(5, 'blocked', 'resumed-original'),
      savingEvent(6, 'settled', 'resumed-original'),
    ];
    const refs = context();
    for (const event of events) handleChatEventEnvelope(event, refs);
    const notes = useChatStore.getState().messages;
    expect(notes.find((note) => note.id === 'journal-save:save-native-2')?.content).toContain(
      '1970-01-01T00:00:01.000Z'
    );
    expect(notes.find((note) => note.id === 'journal-save:save-native-5')?.content).toContain(
      '1970-01-01T00:00:04.000Z'
    );
    expect(notes.some((note) => note.id === 'journal-save:save-native-3')).toBe(false);
    expect(notes.some((note) => note.id === 'journal-save:save-native-4')).toBe(false);
    expect(notes.filter((note) => note.content.includes('现已收到确认'))).toHaveLength(1);
    expect(useChatStore.getState().runStatus).toBe('idle');
    useChatStore.getState().replaceMessages([]);
    expect(useChatStore.getState().transcriptSaves).toEqual({});
    for (const event of [...events, ...events]) handleChatEventEnvelope(event, context());
    expect(useChatStore.getState().messages).toEqual(notes);
    expect(useChatStore.getState().runStatus).toBe('idle');
  });

  it('尾部先到仍从原序号恢复旧确认和交错操作的各自结算', () => {
    const events = [
      savingEvent(1, 'settled', 'confirmed-before'),
      savingEvent(2, 'blocked', 'operation-a'),
      savingEvent(3, 'blocked', 'operation-b'),
      savingEvent(4, 'settled', 'operation-a'),
      savingEvent(5, 'settled', 'operation-b'),
      savingEvent(6, 'blocked', 'new-tail'),
    ];
    const tail = events.at(-1);
    if (!tail) throw new Error('tail missing');
    handleChatEventEnvelope(tail, context());
    for (const event of events) handleChatEventEnvelope(event, context());
    const notes = useChatStore.getState().messages;
    expect(notes.find((note) => note.id === 'journal-save:save-native-6')?.content).toContain(
      '1970-01-01T00:00:05.000Z'
    );
    expect(notes.find((note) => note.id === 'journal-save:save-native-2')?.content).toContain(
      '1970-01-01T00:00:01.000Z'
    );
    expect(notes.filter((note) => note.content.includes('现已收到确认'))).toHaveLength(2);
    expect(useChatStore.getState().runStatus).toBe('idle');
  });

  it('同一operation在其它generation收到确认不能替原保存清债', () => {
    const blocked = savingEvent(1, 'blocked', 'same-operation');
    const foreign = savingEvent(2, 'settled', 'same-operation');
    if (
      foreign.payload.source !== 'agent' ||
      foreign.payload.event.payload.type !== 'transcript_projection_settlement'
    )
      throw new Error('save fixture missing');
    foreign.payload.event.payload.data.generation_id = 'different-generation';
    handleChatEventEnvelope(blocked, context());
    handleChatEventEnvelope(foreign, context());
    expect(
      useChatStore.getState().messages.some((note) => note.content.includes('现已收到确认'))
    ).toBe(false);
    handleChatEventEnvelope(savingEvent(3, 'settled', 'same-operation'), context());
    expect(
      useChatStore.getState().messages.filter((note) => note.content.includes('现已收到确认'))
    ).toHaveLength(1);
  });

  it('retains the original target and owner scope for a live HITL request', () => {
    const base = fixture.at(0);
    if (!base) throw new Error('contract fixture is incomplete');
    const identity = {
      request_id: 'reusable',
      response_target: 'target-current',
      kind: 'input',
      scope: {
        workspace_id: 'workspace-original',
        host_generation: 'generation-original',
        conversation_id: 'conversation-original',
        root_turn_id: 'root-original',
        turn_id: 'turn-original',
      },
    };
    const envelope = {
      ...base,
      workspace_id: 'workspace-original',
      conversation_id: 'conversation-original',
      root_turn_id: 'root-original',
      turn_id: 'turn-original',
      payload: {
        source: 'input_request',
        event: {
          request_id: 'reusable',
          identity,
          prompt: '中文\n🙂',
        },
      },
    } as unknown as ChatEventEnvelope;
    handleChatEventEnvelope(envelope, context());
    expect(useChatStore.getState().pendingHitlRequests.at(0)).toMatchObject({ identity });
  });

  it('keeps a historical HITL event without a target out of the active reply queue', () => {
    const base = fixture.at(0);
    if (!base) throw new Error('contract fixture is incomplete');
    const envelope = {
      ...base,
      payload: {
        source: 'input_request',
        event: {
          request_id: 'old-request',
          prompt: 'expired historical input',
        },
      },
    } as unknown as ChatEventEnvelope;
    handleChatEventEnvelope(envelope, context());
    expect(useChatStore.getState().pendingHitlRequests).toEqual([]);
  });

  it('releases only the exact target and scope when a request id is reused', () => {
    const scope = {
      workspace_id: 'w',
      host_generation: 'host-a',
      conversation_id: 'c',
      root_turn_id: 'root',
      turn_id: 'turn',
    };
    const a = {
      request_id: 'reusable',
      response_target: 'target-a',
      kind: 'input' as const,
      scope,
    };
    const b = { ...a, response_target: 'target-b' };
    const base = fixture.at(0);
    if (!base) throw new Error('contract fixture is incomplete');
    const addressedBase = {
      ...base,
      workspace_id: scope.workspace_id,
      conversation_id: scope.conversation_id,
      root_turn_id: scope.root_turn_id,
      turn_id: scope.turn_id,
    };
    for (const identity of [a, b]) {
      useChatStore
        .getState()
        .enqueueHitlRequest({ kind: 'input', requestId: 'reusable', identity, prompt: 'input' });
    }
    handleChatEventEnvelope(
      {
        ...addressedBase,
        payload: {
          source: 'hitl_request_released',
          event: {
            identity: { ...b, scope: { ...scope, host_generation: 'other-host' } },
          },
        },
      },
      context()
    );
    expect(useChatStore.getState().pendingHitlRequests).toHaveLength(2);
    handleChatEventEnvelope(
      { ...addressedBase, payload: { source: 'hitl_request_released', event: { identity: a } } },
      context()
    );
    expect(useChatStore.getState().pendingHitlRequests).toEqual([
      expect.objectContaining({ identity: b }),
    ]);
  });

  it('does not activate malformed, empty or conflicting request identities', () => {
    const base = fixture.at(0);
    if (!base) throw new Error('contract fixture is incomplete');
    const scope = {
      workspace_id: base.workspace_id,
      host_generation: 'host',
      conversation_id: base.conversation_id,
      root_turn_id: base.root_turn_id,
      turn_id: base.turn_id,
    };
    const identity = { request_id: 'id', response_target: 'target', kind: 'input', scope };
    for (const invalid of [
      { ...identity, response_target: '' },
      { ...identity, response_target: null },
      { ...identity, request_id: 'another' },
      { ...identity, kind: 'approval' },
      { ...identity, scope: { ...scope, conversation_id: 'another' } },
    ]) {
      handleChatEventEnvelope(
        {
          ...base,
          payload: {
            source: 'input_request',
            event: {
              request_id: 'id',
              prompt: 'input',
              identity: invalid,
            },
          },
        } as unknown as ChatEventEnvelope,
        context()
      );
    }
    expect(useChatStore.getState().pendingHitlRequests).toEqual([]);
  });

  it('treats InputLifecycle as a Frontier refresh signal only', () => {
    const base = fixture[0];
    if (!base) throw new Error('contract fixture is incomplete');
    const envelope = {
      ...base,
      workspace_id: 'workspace-input',
      conversation_id: 'conversation-input',
      payload: {
        source: 'input_lifecycle',
        event: { phase: 'persisted' },
      },
    } as ChatEventEnvelope;
    const refresh = vi.fn();

    handleChatEventEnvelope(envelope, { ...context(), onInputLifecycle: refresh });

    expect(refresh).toHaveBeenCalledWith('workspace-input', 'conversation-input');
    expect(useChatStore.getState().messages).toEqual([]);
  });

  it('准确任务绑定事实只刷新原输入Frontier，不制造气泡或投递成功', () => {
    const base = fixture.at(0);
    if (!base) throw new Error('contract fixture is incomplete');
    const event: ConversationInputFact = {
      phase: 'task_run_target_bound',
      attempt: {
        identity: {
          address: {
            workspace_id: 'workspace-input',
            conversation_id: 'conversation-input',
          },
          input_id: 'durable-input',
          revision: 1,
          payload_sha256: 'a'.repeat(64),
        },
        attempt: 1,
        attempt_id: 'durable-attempt',
        turn_id: 'durable-turn',
      },
      run_id: 'original-task-run',
      bound_at_ms: 1,
    };
    const envelope: ChatEventEnvelope = {
      ...base,
      workspace_id: 'workspace-input',
      conversation_id: 'conversation-input',
      root_turn_id: 'durable-root',
      turn_id: 'durable-turn',
      message_id: 'durable-turn',
      payload: { source: 'input_lifecycle', event },
    };
    const refresh = vi.fn();

    handleChatEventEnvelope(envelope, { ...context(), onInputLifecycle: refresh });

    expect(refresh).toHaveBeenCalledTimes(1);
    expect(refresh).toHaveBeenCalledWith('workspace-input', 'conversation-input');
    expect(useChatStore.getState().messages).toEqual([]);
  });

  it('preserves effective invocation and the complete typed tool result through the reducer', () => {
    expect(fixture).toHaveLength(2);
    const [toolCall, toolResult] = fixture;
    if (!toolCall || !toolResult) throw new Error('contract fixture is incomplete');

    expect(toolCall.sequence).toBe(1);
    if (toolCall.payload.source !== 'agent') throw new Error('expected agent tool call');
    const call = toolCall.payload.event.payload;
    if (call.type !== 'tool_call') throw new Error('expected tool_call');
    expect(call.data.invocation.requested_name).toBe('shell');
    expect(call.data.invocation.name).toBe('sandbox_shell');
    expect(call.data.invocation.rewrites).toEqual([
      'intervention_redirect',
      'pre_tool_use_hook',
      'approval',
    ]);

    if (toolResult.payload.source !== 'agent') throw new Error('expected agent tool result');
    const completion = toolResult.payload.event.payload;
    if (completion.type !== 'tool_result') throw new Error('expected tool_result');
    expect(completion.data.result).toMatchObject({
      success: false,
      error: 'command timed out',
      truncated: true,
      failure: {
        category: 'timeout',
        recovery: 'verify_then_retry',
        side_effect: 'possible',
      },
      metadata: {
        artifact_path: '/tmp/tool-output.txt',
        duration_ms: '5000',
      },
    });

    const refs = context();
    handleChatEventEnvelope(toolCall, refs);
    handleChatEventEnvelope(toolResult, refs);
    expect(useChatStore.getState().runStatus).toBe('using_tool');
  });

  it('applies journaled Subagent execution events through the replay handler', () => {
    const base = fixture[0];
    if (!base) throw new Error('contract fixture is incomplete');
    const envelope = {
      ...base,
      workspace_id: 'workspace-1',
      conversation_id: 'conversation-1',
      root_turn_id: 'message-1',
      turn_id: 'message-1',
      message_id: 'message-1',
      payload: {
        source: 'execution',
        event: {
          workspace_id: 'workspace-1',
          conversation_id: 'conversation-1',
          run_id: '',
          scope: 'subagent',
          task_id: null,
          subagent_run_id: 'execution-1',
          event: 'thinking_delta',
          agent: 'explorer',
          payload: { content: 'inspect the repository' },
          framework_event: {
            schema_version: 4,
            event_id: 'evt-2',
            content_hash: 'sha256:2',
            sequence: 2,
            stream_id: 'subagent-stream',
            turn_id: 'message-1',
            message_id: 'message-1',
            execution_id: 'execution-1',
            parent_event_id: null,
            timestamp: '2026-09-05T00:00:02Z',
            parent_agent: 'primary',
            agent_name: 'explorer',
            parent_execution_id: null,
            agent_path: 'primary/explorer',
            task_id: null,
            attempt: 1,
            plan_revision: null,
          },
        },
      },
    } as ChatEventEnvelope;

    handleChatEventEnvelope(envelope, context());

    const run = useSubagentRunStore.getState().runs[subagentRunStoreKey('', 'execution-1')];
    expect(run?.events).toHaveLength(1);
    expect(run?.events[0]?.content).toBe('inspect the repository');
  });

  it('fails closed for an unknown material agent variant', () => {
    const toolCall = fixture[0];
    if (!toolCall || toolCall.payload.source !== 'agent') throw new Error('expected fixture');
    const unknown = {
      ...toolCall,
      payload: {
        source: 'agent',
        event: {
          ...toolCall.payload.event,
          payload: { type: 'future_material_event', data: { value: 'must not disappear' } },
        },
      },
    } as unknown as ChatEventEnvelope;

    expect(() => handleChatEventEnvelope(unknown, context())).toThrow('Unsupported agent event');
  });

  it.each(['failed', 'cancelled'] as const)(
    'preserves a terminal-only %s status instead of finalizing it as completed',
    (status) => {
      const base = fixture[0];
      if (!base) throw new Error('contract fixture is incomplete');
      const assistantId = useChatStore.getState().startAssistantMessage(`assistant-${status}`);
      useChatStore.getState().appendToken(assistantId, 'partial answer');
      const refs = context();
      refs.assistantIdRef.current = assistantId;
      const terminal = {
        ...base,
        sequence: 3,
        payload: { source: 'turn_status', event: { status } },
      } as ChatEventEnvelope;

      handleChatEventEnvelope(terminal, refs);

      const state = useChatStore.getState();
      expect(state.runStatus).toBe(status);
      expect(state.isStreaming).toBe(false);
      expect(state.messages.at(-1)).toMatchObject({
        content: 'partial answer',
        isStreaming: false,
      });
    }
  );

  it('冷历史没有活跃assistant引用时仍显示原失败事实，重放不重复且不伪造终态', () => {
    const base = fixture[0];
    if (!base || base.payload.source !== 'agent') throw new Error('expected agent fixture');
    useChatStore.getState().setRunStatus('running');
    const refs = context();
    const error = {
      ...base,
      payload: {
        source: 'agent',
        event: {
          ...base.payload.event,
          payload: {
            type: 'error',
            data: {
              source: 'react_loop',
              message: '原始Responses解析失败',
              failure: {
                category: 'llm',
                terminal_kind: 'failed',
                retryable: false,
                code: 'llm_invalid_response',
                http_status: null,
                message: '原始Responses解析失败',
              },
            },
          },
        },
      },
    } as ChatEventEnvelope;
    handleChatEventEnvelope(error, refs);
    handleChatEventEnvelope(error, refs);
    expect(useChatStore.getState().messages).toHaveLength(1);
    expect(useChatStore.getState().messages[0]).toMatchObject({
      content: '[Error] react_loop: 原始Responses解析失败',
      timestamp: Date.parse(base.timestamp),
      finalAnswerConfirmed: false,
      isStreaming: false,
    });
    expect(useChatStore.getState().runStatus).toBe('running');
    handleChatEventEnvelope(
      { ...base, payload: { source: 'turn_status', event: { status: 'failed' } } },
      refs
    );
    expect(useChatStore.getState().runStatus).toBe('failed');
  });

  it('多轮冷历史错误按原时间插回，不跑到后续成功回答之后或重复插入', () => {
    useChatStore.getState().replaceMessages([
      { id: 'user-one', role: 'user', content: '第一问', timestamp: 1000 },
      { id: 'empty-one', role: 'assistant', content: '', timestamp: 1100 },
      { id: 'user-two', role: 'user', content: '第二问', timestamp: 4000 },
      { id: 'empty-two', role: 'assistant', content: '', timestamp: 4100 },
      { id: 'user-three', role: 'user', content: '第三问', timestamp: 6000 },
      {
        id: 'answer-three',
        role: 'assistant',
        content: '有效回答',
        timestamp: 6200,
        finalAnswerConfirmed: true,
      },
    ]);
    const first = { id: 'original-error-one', timestamp: 2000 };
    const second = { id: 'original-error-two', timestamp: 5000 };
    useChatStore.getState().appendLocalAssistantNote('[Error] 第二轮失败', second);
    useChatStore.getState().appendLocalAssistantNote('[Error] 第一轮失败', first);
    useChatStore.getState().appendLocalAssistantNote('[Error] 第一轮失败', first);
    expect(useChatStore.getState().messages.map((m) => m.id)).toEqual([
      'user-one',
      'empty-one',
      first.id,
      'user-two',
      'empty-two',
      second.id,
      'user-three',
      'answer-three',
    ]);
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('有效回答');
  });

  it('keeps a typed cancellation failure non-terminal until turn_status settles it', () => {
    const base = fixture[0];
    if (!base || base.payload.source !== 'agent') throw new Error('expected agent fixture');
    const assistantId = useChatStore.getState().startAssistantMessage('assistant-cancelled-error');
    const refs = context();
    refs.assistantIdRef.current = assistantId;
    const cancelledError = {
      ...base,
      payload: {
        source: 'agent',
        event: {
          ...base.payload.event,
          payload: {
            type: 'error',
            data: {
              source: 'agent',
              message: 'cancelled by user',
              failure: {
                category: 'agent',
                terminal_kind: 'cancelled',
                retryable: false,
                code: 'agent_cancelled',
                http_status: null,
                message: 'cancelled by user',
              },
            },
          },
        },
      },
    } as ChatEventEnvelope;

    handleChatEventEnvelope(cancelledError, refs);
    expect(useChatStore.getState().runStatus).toBe('running');
    expect(useChatStore.getState().isStreaming).toBe(true);
    expect(refs.assistantIdRef.current).toBe(assistantId);
    expect(refs.isCancelledRef.current).toBe(true);

    handleChatEventEnvelope(
      {
        ...base,
        sequence: 2,
        payload: { source: 'turn_status', event: { status: 'cancelled' } },
      } as ChatEventEnvelope,
      refs
    );
    expect(useChatStore.getState().runStatus).toBe('cancelled');
    expect(useChatStore.getState().isStreaming).toBe(false);
  });

  it('uses turn_status as the terminal authority over an agent-local cancellation', () => {
    const base = fixture[0];
    if (!base || base.payload.source !== 'agent') throw new Error('expected agent fixture');
    const assistantId = useChatStore.getState().startAssistantMessage('assistant-cancelled');
    const refs = context();
    refs.assistantIdRef.current = assistantId;
    const cancelled = {
      ...base,
      payload: {
        source: 'agent',
        event: { ...base.payload.event, payload: { type: 'cancelled' } },
      },
    } as ChatEventEnvelope;
    const contradictoryTerminal = {
      ...base,
      sequence: 2,
      payload: { source: 'turn_status', event: { status: 'completed' } },
    } as ChatEventEnvelope;
    const lateToken = {
      ...base,
      sequence: 3,
      payload: {
        source: 'agent',
        event: {
          ...base.payload.event,
          schema_version: 5,
          payload: {
            type: 'assistant_message',
            data: {
              block: {
                block_id: 'late-block',
                request_id: 'late-request',
                sequence: 3,
                phase: 'commentary',
                delta: 'late token',
                complete: false,
                provider: null,
                reasoning: null,
              },
            },
          },
        },
      },
    } as ChatEventEnvelope;

    handleChatEventEnvelope(cancelled, refs);
    expect(useChatStore.getState().runStatus).toBe('running');
    expect(useChatStore.getState().isStreaming).toBe(true);
    expect(refs.assistantIdRef.current).toBe(assistantId);
    handleChatEventEnvelope(contradictoryTerminal, refs);
    handleChatEventEnvelope(lateToken, refs);

    expect(useChatStore.getState().runStatus).toBe('completed');
    expect(useChatStore.getState().isStreaming).toBe(false);
    expect(useChatStore.getState().messages.at(-1)?.content).not.toContain('late token');
    expect(
      useChatStore
        .getState()
        .messages.at(-1)
        ?.assistantBlocks?.map((block) => block.content)
        .join('') ?? ''
    ).not.toContain('late token');
    expect(refs.isCancelledRef.current).toBe(false);
  });

  it('lets a failed turn_status override a preceding final answer', () => {
    const base = fixture[0];
    if (!base || base.payload.source !== 'agent') throw new Error('expected agent fixture');
    const assistantId = useChatStore.getState().startAssistantMessage('assistant-failed');
    const refs = context();
    refs.assistantIdRef.current = assistantId;
    handleChatEventEnvelope(
      {
        ...base,
        payload: {
          source: 'agent',
          event: { ...base.payload.event, payload: { type: 'final_answer', data: 'answer' } },
        },
      } as ChatEventEnvelope,
      refs
    );
    expect(useChatStore.getState().runStatus).toBe('running');
    expect(useChatStore.getState().isStreaming).toBe(true);

    handleChatEventEnvelope(
      {
        ...base,
        sequence: 2,
        payload: { source: 'turn_status', event: { status: 'failed' } },
      } as ChatEventEnvelope,
      refs
    );
    expect(useChatStore.getState().runStatus).toBe('failed');
    expect(useChatStore.getState().isStreaming).toBe(false);
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('answer');
  });

  it('delivers a typed command-cell settlement after its foreground turn is terminal', () => {
    const base = fixture[0];
    if (!base) throw new Error('contract fixture is incomplete');
    const refs = context();
    handleChatEventEnvelope(
      {
        ...base,
        sequence: 2,
        payload: { source: 'turn_status', event: { status: 'completed' } },
      } as ChatEventEnvelope,
      refs
    );
    handleChatEventEnvelope(
      {
        ...base,
        sequence: 3,
        payload: {
          source: 'command_cell_settled',
          event: {
            cell: {
              cell_id: 'cell-late',
              name: 'long build',
              command_hash: 'sha256:test',
              turn_id: base.turn_id,
              execution_id: null,
              call_id: 'call-late',
              phase: 'succeeded',
              terminal_cause: 'exited',
              terminal_message: null,
              exit_code: 0,
              artifact_status: 'below_threshold',
              artifact_message: null,
              total_output_bytes: 12,
              output_truncated: false,
              output_excerpt: 'build passed',
              artifact_path: null,
              artifact_sha256: null,
              started_at: '2026-08-22T00:00:00Z',
              finished_at: '2026-08-22T00:00:01Z',
            },
          },
        },
      } as ChatEventEnvelope,
      refs
    );

    expect(useChatStore.getState().messages.at(-1)?.content).toContain(
      'cell-late settled: succeeded'
    );
  });

  it('renders runtime cell truth from a command-cell-watch Ready fact after turn settlement', () => {
    const base = fixture[0];
    if (!base) throw new Error('contract fixture is incomplete');
    const refs = context();
    handleChatEventEnvelope(
      {
        ...base,
        sequence: 2,
        payload: { source: 'turn_status', event: { status: 'completed' } },
      } as ChatEventEnvelope,
      refs
    );
    const terminalCell = {
      cell_id: 'cell-command_cell_watch',
      name: 'long test',
      command_hash: 'sha256:test',
      turn_id: base.turn_id,
      execution_id: 'cell-execution',
      call_id: 'call-command_cell_watch',
      phase: 'succeeded' as const,
      terminal_cause: 'exited' as const,
      terminal_message: null,
      exit_code: 0,
      artifact_status: 'below_threshold' as const,
      artifact_message: null,
      total_output_bytes: 8,
      output_truncated: false,
      output_excerpt: 'passed',
      artifact_path: null,
      artifact_sha256: null,
      started_at: '2026-08-22T00:00:00Z',
      finished_at: '2026-08-22T00:00:01Z',
    };
    handleChatEventEnvelope(
      {
        ...base,
        sequence: 3,
        payload: {
          source: 'command_cell_watch_ready',
          event: {
            result: {
              receipt: {
                execution_id: 'command_cell_watch-execution',
                watch_generation: 1,
                cell_id: 'cell-command_cell_watch',
                workspace_id: base.workspace_id,
                conversation_id: base.conversation_id ?? 'conversation',
                run_id: null,
                root_turn_id: base.root_turn_id,
                state: 'settled',
                started_at: '2026-08-22T00:00:00Z',
                settled_at: '2026-08-22T00:00:01Z',
              },
              cell: terminalCell,
            },
          },
        },
      } as ChatEventEnvelope,
      refs
    );
    expect(useChatStore.getState().messages.at(-1)?.content).toContain(
      'cell cell-command_cell_watch succeeded'
    );
  });

  it('fails closed for an unknown run status', () => {
    const base = fixture[0];
    if (!base) throw new Error('contract fixture is incomplete');
    const unknownStatus = {
      ...base,
      payload: { source: 'turn_status', event: { status: 'future_terminal' } },
    } as unknown as ChatEventEnvelope;

    expect(() => handleChatEventEnvelope(unknownStatus, context())).toThrow(
      'Unsupported chat run status'
    );
  });
});
