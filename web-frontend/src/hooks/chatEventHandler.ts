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
import type { ChatEvent } from '../types/api';

interface EventContext {
  assistantIdRef: React.MutableRefObject<string | null>;
  currentMessageKeyRef: React.MutableRefObject<string | null>;
  currentMessageIdRef: React.MutableRefObject<string | null>;
  isCancelledRef: React.MutableRefObject<boolean>;
  currentThinkingIdRef: React.MutableRefObject<string | null>;
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
      // Observability-only event. Worker trace / trace panels consume the same
      // facts; the chat transcript should not render cache telemetry as text.
      break;
    }
    case 'tool_start': {
      if (ctx.isCancelledRef.current) break;
      // `event.args` is typed `unknown` in ChatEventLike; setToolCall accepts
      // `unknown` — no cast needed.
      if (event.name) store.setToolCall(event.name, event.args ?? undefined);
      break;
    }
    case 'tool_result': {
      if (ctx.isCancelledRef.current) break;
      if (event.name) {
        store.completeToolCall(event.name, (event.result || '') as string, !!event.success);
      }
      break;
    }
    case 'tool_batch_start': {
      if (ctx.isCancelledRef.current) break;
      break;
    }
    case 'tool_batch_end': {
      if (ctx.isCancelledRef.current) break;
      break;
    }
    case 'chart': {
      if (ctx.isCancelledRef.current) break;
      if (event.spec) store.addChartMessage(event.spec);
      break;
    }
    case 'final_answer': {
      if (ctx.isCancelledRef.current) break;
      ctx.isCancelledRef.current = false;
      ctx.currentThinkingIdRef.current = null;
      const id = ctx.assistantIdRef.current;
      if (id) {
        store.finalizeAssistantMessage(id, event.data);
      }
      ctx.assistantIdRef.current = null;
      ctx.currentMessageKeyRef.current = null;
      ctx.currentMessageIdRef.current = null;
      break;
    }
    case 'approval_request': {
      if (ctx.isCancelledRef.current) break;
      // P2-5: 用精确 union 后无需强转, 字段名直接可查。
      store.setApprovalRequest({
        requestId: event.request_id,
        toolName: event.tool_name,
        args: event.args,
        prompt: event.prompt,
      });
      break;
    }
    case 'input_request': {
      if (ctx.isCancelledRef.current) break;
      store.setInputRequest({
        requestId: event.request_id,
        prompt: event.prompt,
      });
      break;
    }
    case 'selection_request': {
      if (ctx.isCancelledRef.current) break;
      store.setSelectionRequest({
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
      store.setRunStatus('failed');
      if (ctx.assistantIdRef.current) {
        store.finalizeAssistantMessage(ctx.assistantIdRef.current, `[Error] ${event.message}`);
      }
      ctx.assistantIdRef.current = null;
      ctx.currentMessageKeyRef.current = null;
      ctx.currentMessageIdRef.current = null;
      break;
    }
    case 'cancelled': {
      store.setRunStatus('cancelled');
      ctx.assistantIdRef.current = null;
      ctx.currentMessageKeyRef.current = null;
      ctx.currentMessageIdRef.current = null;
      ctx.isCancelledRef.current = false;
      break;
    }
    case 'run_status': {
      // Narrow the string status to ChatRunStatus without `as any` (P1-40).
      if (!ctx.isCancelledRef.current) {
        const status = (event.status ?? 'idle') as string;
        const VALID_STATUSES = [
          'idle',
          'running',
          'thinking',
          'using_tool',
          'waiting_approval',
          'waiting_input',
          'completed',
          'failed',
          'cancelled',
        ] as const;
        const validated = VALID_STATUSES.includes(status as (typeof VALID_STATUSES)[number])
          ? (status as (typeof VALID_STATUSES)[number])
          : 'idle';
        store.setRunStatus(validated);
        // Also update taskRuntimeStore so the right rail reflects terminal
        // status. Without 'completed'/'failed'/'cancelled' in the allowlist,
        // updateRunStatus would never see a terminal state and polling
        // would never stop.
        import('../stores/taskRuntimeStore')
          .then(({ useTaskRuntimeStore }) => {
            const taskStore = useTaskRuntimeStore.getState();
            if (taskStore.activeRun) {
              taskStore.updateRunStatus(validated);
            }
          })
          .catch(() => {});
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
      if (ctx.assistantIdRef.current && !ctx.isCancelledRef.current) {
        store.finalizeAssistantMessage(ctx.assistantIdRef.current, '');
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
