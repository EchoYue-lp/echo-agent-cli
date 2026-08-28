/**
 * Shared chat event handler — eliminates the ~120-line switch duplication
 * between useTauriChat and useWebSocket (Phase 6.3).
 *
 * Both transports produce the same logical events; this function contains the
 * single canonical dispatch logic.  Each transport hook calls handleChatEvent
 * with its own store/ref accessors.
 */

import { useChatStore } from '../stores/chatStore';
import type { AgentEvent, ChatEvent, ChatEventEnvelope } from '../types/api';

interface EventContext {
  assistantIdRef: React.MutableRefObject<string | null>;
  currentMessageKeyRef: React.MutableRefObject<string | null>;
  currentMessageIdRef: React.MutableRefObject<string | null>;
  isCancelledRef: React.MutableRefObject<boolean>;
  currentThinkingIdRef: React.MutableRefObject<string | null>;
  onInputLifecycle?: (workspaceId: string, conversationId: string) => void;
}

export function handleChatEventEnvelope(envelope: ChatEventEnvelope, ctx: EventContext): void {
  const payload = envelope.payload;
  switch (payload.source) {
    case 'agent': {
      handleAgentEvent(payload.event.payload, ctx);
      break;
    }
    case 'turn_status': {
      handleChatEvent({ type: 'run_status', status: payload.event.status }, ctx);
      break;
    }
    case 'execution_path':
      handleChatEvent({ type: 'execution_path', ...payload.event }, ctx);
      break;
    case 'interrupt':
      handleChatEvent({ type: 'interrupt_prompt', ...payload.event }, ctx);
      break;
    case 'input_lifecycle':
      if (envelope.conversation_id) {
        ctx.onInputLifecycle?.(envelope.workspace_id, envelope.conversation_id);
      }
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
    case 'command_cell_started':
      handleChatEvent(
        {
          type: 'notice',
          level: 'info',
          code: 'command_cell_started',
          message: `Command cell ${payload.event.cell.cell_id} started: ${payload.event.cell.name}`,
        },
        ctx
      );
      break;
    case 'command_cell_settled': {
      const cell = payload.event.cell;
      handleChatEvent(
        {
          type: 'notice',
          level: cell.phase === 'succeeded' ? 'info' : 'warning',
          code: 'command_cell_settled',
          message: `Command cell ${cell.cell_id} settled: ${cell.phase}`,
        },
        ctx
      );
      break;
    }
    case 'awaiter_result_ready': {
      const result = payload.event.result;
      handleChatEvent(
        {
          type: 'notice',
          level: result.cell.phase === 'succeeded' ? 'info' : 'warning',
          code: 'awaiter_result_ready',
          message: `Awaiter ${result.receipt.execution_id}: cell ${result.cell.cell_id} ${result.cell.phase}`,
        },
        ctx
      );
      break;
    }
    case 'awaiter_result_delivery_started':
    case 'awaiter_result_acknowledged':
      break;
    case 'execution':
      // The exact payload remains in the durable envelope. The dedicated
      // execution projection updates the TaskRuntime store.
      break;
    default:
      assertNever(payload, 'chat driver event');
  }
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
        store.projectAssistantError(ctx.assistantIdRef.current, event.message);
      }
      break;
    }
    case 'cancelled': {
      // Reject late deltas, but leave lifecycle state and control refs intact
      // until the canonical turn_status fact closes the turn.
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
      if (terminal) settleTurnProjection(store, ctx);
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
      store.appendLocalAssistantNote(`[Info] Execution path: ${event.observed_path}`);
      break;
    }
    case 'interrupt_prompt': {
      // The typed send admission response owns the decision callback. This
      // journal event is replay evidence and must not create an actionless
      // duplicate dialog.
      break;
    }
  }
}

function settleTurnProjection(store: ReturnType<typeof useChatStore.getState>, ctx: EventContext) {
  store.clearHitlRequests();
  if (ctx.assistantIdRef.current) {
    store.settleAssistantMessage(ctx.assistantIdRef.current);
  }
  ctx.assistantIdRef.current = null;
  ctx.currentMessageKeyRef.current = null;
  ctx.currentMessageIdRef.current = null;
  ctx.isCancelledRef.current = false;
  ctx.currentThinkingIdRef.current = null;
}
