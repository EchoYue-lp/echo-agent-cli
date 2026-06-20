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

interface ChatEventLike {
  type: string;
  data?: string;
  name?: string;
  args?: unknown;
  result?: string;
  success?: boolean;
  spec?: unknown;
  status?: string;
  message_key?: string;
  conversation_id?: string | null;
  run_id?: string;
  goal?: string;
  domain_profile?: string;
  route?: string;
  interaction_mode?: string;
  permission_mode?: string;
  approval_policy?: string;
  route_reason?: string;
  confidence?: number;
  auto_execute?: boolean;
  suggested_workers?: string[];
  route_signals?: string[];
  classification_signals?: string[];
}

interface EventContext {
  assistantIdRef: React.MutableRefObject<string | null>;
  currentMessageKeyRef: React.MutableRefObject<string | null>;
  currentMessageIdRef: React.MutableRefObject<string | null>;
  isCancelledRef: React.MutableRefObject<boolean>;
  currentThinkingIdRef: React.MutableRefObject<string | null>;
}

export function handleChatEvent(
  event: ChatEventLike,
  ctx: EventContext,
): void {
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
        store.finalizeAssistantMessage(id, (event.data || event.result || '') as string);
      }
      ctx.assistantIdRef.current = null;
      ctx.currentMessageKeyRef.current = null;
      ctx.currentMessageIdRef.current = null;
      break;
    }
    case 'approval_request': {
      if (ctx.isCancelledRef.current) break;
      // Map snake_case event fields to the camelCase ApprovalRequest shape.
      store.setApprovalRequest({
        requestId: (event as { request_id?: string }).request_id ?? '',
        toolName: (event as { tool_name?: string }).tool_name ?? '',
        args: event.args,
        prompt: (event as { prompt?: string }).prompt,
      });
      break;
    }
    case 'input_request': {
      if (ctx.isCancelledRef.current) break;
      store.setInputRequest({
        requestId: (event as { request_id?: string }).request_id ?? '',
        prompt: (event as { prompt?: string }).prompt,
      });
      break;
    }
    case 'selection_request': {
      if (ctx.isCancelledRef.current) break;
      store.setSelectionRequest({
        requestId: (event as { request_id?: string }).request_id ?? '',
        prompt: (event as { prompt?: string }).prompt ?? '',
        options: (event as { options?: string[] }).options ?? [],
        taskId: (event as { task_id?: string }).task_id,
        context: event.spec,
      });
      break;
    }
    case 'plan_ready': {
      if (ctx.isCancelledRef.current) break;
      if (event.run_id) {
        void useTaskRuntimeStore.getState().notifyPlanReady(event.run_id, {
          goal: event.goal,
          domainProfile: event.domain_profile,
          route: event.route,
          interactionMode: event.interaction_mode,
          permissionMode: event.permission_mode,
          approvalPolicy: event.approval_policy,
          routeReason: event.route_reason,
          confidence: event.confidence,
          autoExecute: event.auto_execute,
          suggestedWorkers: event.suggested_workers ?? [],
          routeSignals: event.route_signals ?? [],
          classificationSignals: event.classification_signals ?? [],
        });
      }
      ctx.currentThinkingIdRef.current = null;
      const id = ctx.assistantIdRef.current;
      if (id) {
        const workerCount = event.suggested_workers?.length ?? 0;
        const workerText = workerCount > 0 ? `，${workerCount} 个 worker 已接入` : '';
        const content = event.auto_execute
          ? `已进入 TaskRuntime 并开始执行${workerText}。下面会展示触发原因、worker 执行过程和最终结果；右侧仅保留任务状态概览。`
          : `任务计划已生成，等待确认后执行${workerText}。下面会展示触发原因、worker 执行过程和最终结果；右侧仅保留任务状态概览。`;
        store.handoffToTaskRuntime(id, content, event.auto_execute === true);
      } else {
        store.setStreaming(false);
      }
      ctx.assistantIdRef.current = null;
      ctx.currentMessageKeyRef.current = null;
      ctx.currentMessageIdRef.current = null;
      break;
    }
    case 'error': {
      ctx.isCancelledRef.current = true;
      store.setRunStatus('failed');
      const message = (event as { message?: string }).message ?? '请求失败';
      if (ctx.assistantIdRef.current) {
        store.finalizeAssistantMessage(ctx.assistantIdRef.current, `[Error] ${message}`);
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
          'idle', 'running', 'thinking', 'using_tool',
          'waiting_approval', 'waiting_input',
        ] as const;
        const validated = VALID_STATUSES.includes(status as (typeof VALID_STATUSES)[number])
          ? (status as (typeof VALID_STATUSES)[number])
          : 'idle';
        store.setRunStatus(validated);
      }
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
