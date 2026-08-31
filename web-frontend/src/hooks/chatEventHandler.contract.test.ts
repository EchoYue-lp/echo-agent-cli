import { beforeEach, describe, expect, it, vi } from 'vitest';
import fixtureText from '../fixtures/chat-event-envelope-v4.json?raw';
import { useChatStore } from '../stores/chatStore';
import type { ChatEventEnvelope } from '../types/api';
import { handleChatEventEnvelope } from './chatEventHandler';
import { resetChatEventCursorsForTest } from './chatEventSequencer';

const fixture = JSON.parse(fixtureText) as ChatEventEnvelope[];

const context = (): Parameters<typeof handleChatEventEnvelope>[1] => ({
  assistantIdRef: { current: null },
  currentMessageKeyRef: { current: null },
  currentMessageIdRef: { current: null },
  isCancelledRef: { current: false },
  currentThinkingIdRef: { current: null },
});

describe('canonical chat event v4 contract', () => {
  beforeEach(() => {
    useChatStore.getState().clearMessages();
    resetChatEventCursorsForTest();
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
        event: { ...base.payload.event, payload: { type: 'token', data: 'late token' } },
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
