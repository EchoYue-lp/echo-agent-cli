/**
 * Shared chat event handler — eliminates the ~120-line switch duplication
 * between useTauriChat and useWebSocket (Phase 6.3).
 *
 * Both transports produce the same logical events; this function contains the
 * single canonical dispatch logic.  Each transport hook calls handleChatEvent
 * with its own store/ref accessors.
 */

import { useChatStore } from '../stores/chatStore';
import { useTaskRuntimeStore } from '../stores/taskRuntimeStore';
import type { AgentEvent, ChatEvent, ChatEventEnvelope, ChatRunStatus } from '../types/api';
import { recordTerminalStatusForTurn, terminalStatusForTurn } from './chatEventSequencer';

interface EventContext {
  assistantIdRef: React.MutableRefObject<string | null>;
  currentMessageKeyRef: React.MutableRefObject<string | null>;
  currentMessageIdRef: React.MutableRefObject<string | null>;
  isCancelledRef: React.MutableRefObject<boolean>;
  currentThinkingIdRef: React.MutableRefObject<string | null>;
}

export function handleChatEventEnvelope(envelope: ChatEventEnvelope, ctx: EventContext): void {
  const payload = envelope.payload;
  const terminalAlreadyEstablished =
    terminalStatusForTurn(envelope.stream_id, envelope.turn_id) !== null;
  if (
    terminalAlreadyEstablished &&
    payload.source !== 'agent' &&
    payload.source !== 'turn_status'
  ) {
    return;
  }
  switch (payload.source) {
    case 'agent': {
      if (terminalAlreadyEstablished) return;
      handleAgentEvent(payload.event.payload, ctx);
      break;
    }
    case 'turn_status': {
      const terminalStatus = turnTerminalStatus(payload.event.status);
      if (!terminalStatus && terminalAlreadyEstablished) return;
      if (
        terminalStatus &&
        !recordTerminalStatusForTurn(envelope.stream_id, envelope.turn_id, terminalStatus)
      ) {
        handleChatEvent({ type: 'done' }, ctx);
        return;
      }
      handleChatEvent({ type: 'run_status', status: payload.event.status }, ctx);
      if (terminalStatus) handleChatEvent({ type: 'done' }, ctx);
      break;
    }
    case 'execution_path':
      handleChatEvent({ type: 'execution_path', ...payload.event }, ctx);
      break;
    case 'interrupt':
      handleChatEvent({ type: 'interrupt_prompt', ...payload.event }, ctx);
      break;
    case 'approval_request':
      handleChatEvent({ type: 'approval_request', ...payload.event }, ctx);
      break;
    case 'input_request':
      handleChatEvent({ type: 'input_request', ...payload.event }, ctx);
      break;
    case 'selection_request':
      handleChatEvent({ type: 'selection_request', ...payload.event }, ctx);
      break;
    case 'context_compressed':
      handleChatEvent({ type: 'context_compressed', ...payload.event }, ctx);
      break;
    case 'execution':
      // The exact payload remains in the durable envelope. The dedicated
      // execution projection updates the TaskRuntime store.
      break;
    default:
      assertNever(payload, 'chat driver event');
  }
}

function turnTerminalStatus(
  status: ChatRunStatus
): Extract<ChatRunStatus, 'completed' | 'failed' | 'cancelled'> | null {
  return status === 'completed' || status === 'failed' || status === 'cancelled' ? status : null;
}

function handleAgentEvent(event: AgentEvent, ctx: EventContext): void {
  switch (event.type) {
    case 'token':
      handleChatEvent({ type: 'token', data: event.data }, ctx);
      break;
    case 'think_start':
      handleChatEvent({ type: 'run_status', status: 'thinking' }, ctx);
      handleChatEvent({ type: 'thinking_start' }, ctx);
      break;
    case 'think_end':
      handleChatEvent({ type: 'thinking_end', ...event.data }, ctx);
      break;
    case 'llm_usage':
      handleChatEvent({ type: 'llm_usage', ...event.data }, ctx);
      break;
    case 'context_compressed':
      handleChatEvent({ type: 'context_compressed', ...event.data }, ctx);
      break;
    case 'tool_call':
      handleChatEvent({ type: 'run_status', status: 'using_tool' }, ctx);
      break;
    case 'tool_batch_start':
      handleChatEvent({ type: 'tool_batch_start', tool_count: event.data.tool_count }, ctx);
      break;
    case 'tool_batch_end':
      handleChatEvent({ type: 'tool_batch_end' }, ctx);
      break;
    case 'chart':
      handleChatEvent({ type: 'chart', spec: event.data.spec }, ctx);
      break;
    case 'final_answer':
      handleChatEvent({ type: 'final_answer', data: event.data }, ctx);
      break;
    case 'cancelled':
      handleChatEvent({ type: 'cancelled' }, ctx);
      break;
    case 'error':
      if (event.data.failure.terminal_kind === 'cancelled') {
        handleChatEvent({ type: 'cancelled' }, ctx);
      } else {
        handleChatEvent(
          { type: 'error', message: `${event.data.source}: ${event.data.message}` },
          ctx
        );
      }
      break;
    case 'budget_decision':
    case 'guard_triggered':
    case 'memory_recalled':
    case 'safety_notice':
    case 'parameter_error':
      handleChatEvent(
        {
          type: 'notice',
          level: 'info',
          code: event.type,
          message: JSON.stringify(event.data),
        },
        ctx
      );
      break;
    case 'tool_stream':
    case 'tool_result':
      // Tool facts are rendered from the already-persisted detail projection.
      break;
    default:
      assertNever(event, 'agent event');
  }
}

function assertNever(value: never, label: string): never {
  throw new Error(`Unsupported ${label}: ${JSON.stringify(value)}`);
}

export function handleChatEvent(event: ChatEvent, ctx: EventContext): void {
  const store = useChatStore.getState();

  switch (event.type) {
    case 'token': {
      if (ctx.isCancelledRef.current) break;
      const id = ctx.assistantIdRef.current;
      if (id && event.data) {
        // Route the token by thinking state: tokens arriving between
        // thinking_start and thinking_end are the model's reasoning / per-step
        // thought (emitted by the backend as ThinkStart→Token→ThinkEnd). They
        // must go into thinkingSegments so they render in the collapsible
        // "思考与执行" block, NOT into message.content (which is reserved for
        // the final answer). Without this split, the thought is silently
        // merged into the answer text and the thinking block stays empty.
        if (ctx.currentThinkingIdRef.current) {
          store.appendThinking(id, event.data);
        } else {
          store.appendToken(id, event.data);
        }
      }
      break;
    }
    case 'thinking_start': {
      if (ctx.isCancelledRef.current) break;
      const id = ctx.assistantIdRef.current;
      if (id) {
        store.startThinkingSegment(id);
        ctx.currentThinkingIdRef.current = id;
      }
      break;
    }
    case 'thinking_end': {
      if (ctx.isCancelledRef.current) break;
      // Close the thinking window so subsequent tokens route to content again.
      ctx.currentThinkingIdRef.current = null;
      break;
    }
    case 'llm_usage': {
      // 更新上下文窗口占用快照（不作为聊天消息渲染，仅驱动 footer 指示器）。
      // 对齐 Claude Code statusline：用真实 prompt_tokens 表示当前上下文长度。
      // usage_reported=false 时不更新（避免闪 0 / 污染命中率）。
      if (event.usage_reported === false) {
        break;
      }
      store.setContextWindow({
        inputTokens: event.prompt_tokens,
        cachedTokens: event.cached_prompt_tokens,
        cacheCreationTokens: event.cache_creation_prompt_tokens,
        outputTokens: event.completion_tokens,
        usageReported: true,
      });
      store.recordUsage(event.prompt_tokens, event.cached_prompt_tokens);
      break;
    }
    case 'context_compressed': {
      // 方案 A：压缩后 Snapshot 置空，Accumulator 保留（会话级缓存率跨压缩）。
      store.clearContextWindow();
      break;
    }
    case 'tool_batch_start': {
      if (ctx.isCancelledRef.current) break;
      store.startToolBatch(event.tool_count ?? 0);
      break;
    }
    case 'tool_batch_end': {
      if (ctx.isCancelledRef.current) break;
      store.endToolBatch();
      break;
    }
    case 'chart': {
      if (ctx.isCancelledRef.current) break;
      if (event.spec) store.addChartMessage(event.spec);
      break;
    }
    case 'final_answer': {
      if (ctx.isCancelledRef.current) break;
      store.clearHitlRequests();
      ctx.currentThinkingIdRef.current = null;
      const id = ctx.assistantIdRef.current;
      if (id) {
        store.applyFinalAnswer(id, event.data);
      }
      break;
    }
    case 'approval_request': {
      if (ctx.isCancelledRef.current) break;
      // P2-5: 用精确 union 后无需强转, 字段名直接可查。
      store.enqueueHitlRequest({
        kind: 'approval',
        requestId: event.request_id,
        toolName: event.tool_name,
        args: event.args,
        prompt: event.prompt,
      });
      break;
    }
    case 'input_request': {
      if (ctx.isCancelledRef.current) break;
      store.enqueueHitlRequest({
        kind: 'input',
        requestId: event.request_id,
        prompt: event.prompt,
      });
      break;
    }
    case 'selection_request': {
      if (ctx.isCancelledRef.current) break;
      store.enqueueHitlRequest({
        kind: 'selection',
        requestId: event.request_id,
        prompt: event.prompt,
        options: event.options,
        taskId: event.task_id ?? undefined,
        context: event.context,
      });
      break;
    }
    case 'error': {
      ctx.isCancelledRef.current = true;
      if (ctx.assistantIdRef.current) {
        store.failAssistantMessage(ctx.assistantIdRef.current, event.message);
      } else {
        store.setRunStatus('failed');
      }
      ctx.assistantIdRef.current = null;
      break;
    }
    case 'cancelled': {
      store.setRunStatus('cancelled');
      if (ctx.assistantIdRef.current) {
        store.settleAssistantMessage(ctx.assistantIdRef.current);
      }
      ctx.assistantIdRef.current = null;
      // Preserve the local guard until TurnStatus closes the stream. This
      // rejects late deltas while still allowing the canonical status to win.
      ctx.isCancelledRef.current = true;
      break;
    }
    case 'run_status': {
      const terminal = ['completed', 'failed', 'cancelled'].includes(event.status);
      if (!ctx.isCancelledRef.current || terminal) {
        switch (event.status) {
          case 'idle':
          case 'running':
          case 'thinking':
          case 'using_tool':
          case 'waiting_approval':
          case 'waiting_input':
          case 'completed':
          case 'failed':
          case 'cancelled':
            store.setRunStatus(event.status);
            break;
          default:
            assertNever(event.status, 'chat run status');
        }
      }
      break;
    }
    case 'notice': {
      if (ctx.isCancelledRef.current) break;
      const prefix =
        event.level === 'error' ? '[Error]' : event.level === 'warning' ? '[Warning]' : '[Info]';
      store.appendLocalAssistantNote(`${prefix} ${event.message}`);
      break;
    }
    case 'execution_path': {
      if (event.requested_mode !== event.observed_path) {
        store.appendLocalAssistantNote(
          `[Info] Execution path: ${event.requested_mode} -> ${event.observed_path}`
        );
      }
      break;
    }
    case 'interrupt_prompt': {
      // An in-progress run was detected — the GUI should show a dialog
      // letting the user choose: resume / edit-and-resume / abandon.
      useTaskRuntimeStore.getState().openInterruptPrompt({
        runId: event.run_id,
        goal: event.goal,
        newMessage: event.new_message,
      });
      break;
    }
    case 'done': {
      store.clearHitlRequests();
      if (ctx.assistantIdRef.current) {
        store.settleAssistantMessage(ctx.assistantIdRef.current);
      }
      ctx.assistantIdRef.current = null;
      ctx.currentMessageKeyRef.current = null;
      ctx.currentMessageIdRef.current = null;
      ctx.isCancelledRef.current = false;
      // Also close any thinking segment
      if (ctx.currentThinkingIdRef.current) {
        ctx.currentThinkingIdRef.current = null;
      }
      break;
    }
  }
}
