import { useRef, useCallback, useEffect, useState } from 'react';
import { useChatStore } from '../stores/chatStore';
import { useTaskRuntimeStore } from '../stores/taskRuntimeStore';
import { isTauri } from '../lib/tauri-bridge';
import type { ClientMessage, ServerMessage, Attachment } from '../types/api';

export type ConnectionStatus = 'connected' | 'disconnected' | 'connecting';

const INITIAL_RECONNECT_MS = 1000;
const MAX_RECONNECT_MS = 30000;
const MAX_RECONNECT_ATTEMPTS = 20;
const HEARTBEAT_INTERVAL_MS = 15000;
const HEARTBEAT_TIMEOUT_MS = 10000;

export function useWebSocket() {
  const wsRef = useRef<WebSocket | null>(null);
  const assistantIdRef = useRef<string | null>(null);
  const currentMessageIdRef = useRef<string | null>(null);
  const inThinkingRef = useRef(false);
  const isCancelledRef = useRef(false);
  const reconnectTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const heartbeatTimer = useRef<ReturnType<typeof setInterval>>(undefined);
  const heartbeatTimeout = useRef<ReturnType<typeof setTimeout>>(undefined);
  const messageQueue = useRef<ClientMessage[]>([]);
  const retryCount = useRef(0);
  /// Tracks whether the current close was triggered by a heartbeat timeout.
  /// Heartbeat-detected disconnects do NOT consume a reconnect attempt — they
  /// reflect an already-broken connection, not a server-side rejection.
  const isHeartbeatClose = useRef(false);
  const [connectionStatus, setConnectionStatus] = useState<ConnectionStatus>('connecting');

  const getReconnectDelay = useCallback(() => {
    const delay = Math.min(
      INITIAL_RECONNECT_MS * Math.pow(2, retryCount.current),
      MAX_RECONNECT_MS
    );
    return delay;
  }, []);

  const connect = useCallback(() => {
    // Guard: skip if already open or connecting
    if (
      wsRef.current?.readyState === WebSocket.OPEN ||
      wsRef.current?.readyState === WebSocket.CONNECTING
    )
      return;

    // Close stale socket before creating new one
    if (wsRef.current) {
      wsRef.current.onopen = null;
      wsRef.current.onmessage = null;
      wsRef.current.onclose = null;
      wsRef.current.onerror = null;
      wsRef.current.close();
    }

    const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const ws = new WebSocket(`${protocol}//${location.host}/ws/chat`);
    wsRef.current = ws;
    setConnectionStatus('connecting');
    ws.onopen = () => {
      console.log('[WS] connected');
      setConnectionStatus('connected');
      retryCount.current = 0;

      // Start heartbeat to detect silent disconnects
      clearHeartbeat();
      heartbeatTimer.current = setInterval(() => {
        if (wsRef.current?.readyState === WebSocket.OPEN) {
          wsRef.current.send(JSON.stringify({ type: 'ping' }));
          heartbeatTimeout.current = setTimeout(() => {
            console.warn('[WS] Heartbeat timeout, closing connection');
            isHeartbeatClose.current = true;
            wsRef.current?.close();
          }, HEARTBEAT_TIMEOUT_MS);
        }
      }, HEARTBEAT_INTERVAL_MS);

      // Flush queued messages (with TTL: discard messages older than 30s)
      const now = Date.now();
      const QUEUE_MAX_AGE_MS = 30_000;
      const queued = messageQueue.current.filter(
        (m) => (m as any)._ts && (now - (m as any)._ts) < QUEUE_MAX_AGE_MS
      );
      messageQueue.current = [];
      for (const msg of queued) {
        ws.send(JSON.stringify(msg));
      }
    };

    ws.onmessage = (ev) => {
      let msg: ServerMessage;
      try {
        msg = JSON.parse(ev.data);
      } catch {
        console.error('[WS] Failed to parse message:', ev.data);
        return;
      }
      const store = useChatStore.getState();
      if (msg.type !== 'pong' && 'id' in msg && msg.id && currentMessageIdRef.current !== msg.id) {
        return;
      }

      switch (msg.type) {
        case 'token': {
          if (isCancelledRef.current) break;
          if (!assistantIdRef.current) {
            assistantIdRef.current = store.startAssistantMessage();
          }
          if (!inThinkingRef.current) {
            inThinkingRef.current = true;
            store.setThinking(true);
            store.startThinkingSegment(assistantIdRef.current);
          }
          store.appendThinking(assistantIdRef.current, msg.data);
          break;
        }
        case 'thinking_start':
          if (isCancelledRef.current) break;
          inThinkingRef.current = true;
          store.setThinking(true);
          if (!assistantIdRef.current) {
            assistantIdRef.current = store.startAssistantMessage();
          }
          store.startThinkingSegment(assistantIdRef.current);
          break;
        case 'thinking_end':
          inThinkingRef.current = false;
          store.setThinking(false);
          break;
        case 'tool_start':
          if (isCancelledRef.current) break;
          inThinkingRef.current = false;
          store.setThinking(false);
          store.setRunStatus('using_tool');
          store.setToolCall(msg.name, msg.args);
          break;
        case 'tool_result':
          if (isCancelledRef.current) break;
          store.completeToolCall(msg.name, msg.result, msg.success);
          store.setRunStatus('running');
          break;
        case 'tool_batch_start':
          if (isCancelledRef.current) break;
          store.startToolBatch(msg.tool_count);
          break;
        case 'tool_batch_end':
          if (isCancelledRef.current) break;
          store.endToolBatch();
          break;
        case 'final_answer': {
          if (!isCancelledRef.current && assistantIdRef.current) {
            store.finalizeAssistantMessage(assistantIdRef.current, msg.data);
          }
          inThinkingRef.current = false;
          assistantIdRef.current = null;
          currentMessageIdRef.current = null;
          isCancelledRef.current = false;
          break;
        }
        case 'approval_request':
          if (isCancelledRef.current) break;
          store.setApprovalRequest({
            requestId: msg.request_id,
            toolName: msg.tool_name,
            args: msg.args,
            prompt: msg.prompt,
          });
          break;
        case 'input_request':
          if (isCancelledRef.current) break;
          store.setInputRequest({ requestId: msg.request_id, prompt: msg.prompt });
          break;
        case 'selection_request':
          if (isCancelledRef.current) break;
          store.setSelectionRequest({
            requestId: msg.request_id,
            prompt: msg.prompt,
            options: msg.options,
            taskId: msg.task_id ?? undefined,
            context: msg.context,
            phase: msg.phase ?? undefined,
          });
          break;
        case 'pong':
          // Heartbeat response received, clear timeout
          if (heartbeatTimeout.current) {
            clearTimeout(heartbeatTimeout.current);
            heartbeatTimeout.current = undefined;
          }
          break;
        case 'chart':
          if (isCancelledRef.current) break;
          store.addChartMessage(msg.spec);
          break;
        case 'error': {
          if (!isCancelledRef.current && assistantIdRef.current) {
            store.finalizeAssistantMessage(assistantIdRef.current, `[Error] ${msg.message}`);
          }
          store.setRunStatus('failed');
          assistantIdRef.current = null;
          currentMessageIdRef.current = null;
          isCancelledRef.current = false;
          break;
        }
        case 'cancelled':
          store.setRunStatus('cancelled');
          assistantIdRef.current = null;
          currentMessageIdRef.current = null;
          isCancelledRef.current = false;
          break;
        case 'run_status':
          if (!isCancelledRef.current) store.setRunStatus(msg.status);
          break;
        case 'done':
          // If no final_answer was received, finalize with empty content.
          if (assistantIdRef.current && !isCancelledRef.current) {
            store.finalizeAssistantMessage(assistantIdRef.current, '');
          }
          assistantIdRef.current = null;
          currentMessageIdRef.current = null;
          isCancelledRef.current = false;
          break;
        case 'plan_ready':
          // A complex input was routed to TaskRuntime and a plan was generated.
          // Hand it to the TaskRuntime store so the right-rail panel renders the
          // plan + approval actions (was missing in Web mode, see P1-7).
          useTaskRuntimeStore.getState().notifyPlanReady(msg.run_id, {
            goal: msg.goal,
            domainProfile: msg.domain_profile,
            route: msg.route,
            interactionMode: msg.interaction_mode,
            permissionMode: msg.permission_mode,
            approvalPolicy: msg.approval_policy,
            routeReason: msg.route_reason,
            confidence: msg.confidence,
            autoExecute: msg.auto_execute,
            plannedWorkers: msg.planned_workers ?? [],
            suggestedWorkers: msg.suggested_workers ?? [],
            activeSkills: msg.active_skills ?? [],
            routeSignals: msg.route_signals ?? [],
            classificationSignals: msg.classification_signals ?? [],
          });
          break;
      }
    };

    ws.onclose = () => {
      clearHeartbeat();
      // Heartbeat timeouts detect an already-broken connection — they are
      // NOT a fresh disconnect and should not consume a reconnect attempt.
      if (!isHeartbeatClose.current) {
        retryCount.current += 1;
      }
      isHeartbeatClose.current = false;

      if (retryCount.current > MAX_RECONNECT_ATTEMPTS) {
        console.error(`[WS] Max reconnect attempts (${MAX_RECONNECT_ATTEMPTS}) reached, giving up`);
        setConnectionStatus('disconnected');
        return;
      }

      const delay = getReconnectDelay();
      console.log(
        `[WS] disconnected, reconnecting in ${delay}ms (attempt ${retryCount.current}/${MAX_RECONNECT_ATTEMPTS})`
      );
      setConnectionStatus('disconnected');
      reconnectTimer.current = setTimeout(connect, delay);
    };

    ws.onerror = () => {
      console.error('[WS] error');
      setConnectionStatus('disconnected');
    };
  }, [getReconnectDelay]);

  const clearHeartbeat = () => {
    if (heartbeatTimer.current) {
      clearInterval(heartbeatTimer.current);
      heartbeatTimer.current = undefined;
    }
    if (heartbeatTimeout.current) {
      clearTimeout(heartbeatTimeout.current);
      heartbeatTimeout.current = undefined;
    }
  };

  const send = useCallback((msg: ClientMessage): boolean => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      wsRef.current.send(JSON.stringify(msg));
      return true;
    }
    // Queue message for when reconnection succeeds (capped)
    const MAX_QUEUE = 100;
    if (msg.type !== 'ping' && msg.type !== 'cancel') {
      if (messageQueue.current.length >= MAX_QUEUE) {
        messageQueue.current.shift(); // drop oldest
      }
      // Attach timestamp for TTL-based eviction on drain (P1-2.13)
      (msg as any)._ts = Date.now();
      messageQueue.current.push(msg);
    }
    return false;
  }, []);

  const sendMessage = useCallback(
    (text: string, attachments?: Attachment[]) => {
      const store = useChatStore.getState();
      const displayAttachments = attachments?.map((a) => ({
        name: a.name,
        mime_type: a.mime_type,
        url: `data:${a.mime_type};base64,${a.data}`,
        size: a.size,
      }));
      store.addUserMessage(text || '(附件)', displayAttachments);
      const messageId =
        typeof crypto !== 'undefined' && 'randomUUID' in crypto
          ? crypto.randomUUID()
          : `chat-${Date.now()}-${Math.random().toString(36).slice(2)}`;
      currentMessageIdRef.current = messageId;

      if (!send({ type: 'message', id: messageId, data: text, attachments })) {
        currentMessageIdRef.current = null;
        store.markCancelled();
      } else {
        isCancelledRef.current = false;
        assistantIdRef.current = store.startAssistantMessage();
        store.setRunStatus('running');
      }
    },
    [send]
  );

  const sendApproval = useCallback(
    (requestId: string, approved: boolean, reason?: string) => {
      send({
        type: 'approval_response',
        id: currentMessageIdRef.current ?? undefined,
        request_id: requestId,
        approved,
        reason,
      });
      useChatStore.getState().setApprovalRequest(null);
      useChatStore.getState().setRunStatus('running');
    },
    [send]
  );

  const sendInput = useCallback(
    (requestId: string, text: string) => {
      send({
        type: 'input_response',
        id: currentMessageIdRef.current ?? undefined,
        request_id: requestId,
        text,
      });
      useChatStore.getState().setInputRequest(null);
      useChatStore.getState().setRunStatus('running');
    },
    [send]
  );

  const sendSelection = useCallback(
    (requestId: string, selection: string, instructions?: string) => {
      send({
        type: 'selection_response',
        id: currentMessageIdRef.current ?? undefined,
        request_id: requestId,
        selection,
        instructions,
      });
      useChatStore.getState().setSelectionRequest(null);
      useChatStore.getState().setRunStatus('running');
    },
    [send]
  );

  const cancel = useCallback(() => {
    useChatStore.getState().markCancelled();
    assistantIdRef.current = null;
    const messageId = currentMessageIdRef.current;
    currentMessageIdRef.current = null;
    isCancelledRef.current = true;
    send({ type: 'cancel', id: messageId ?? undefined });
  }, [send]);

  /// Manual reconnect — resets the retry counter so the user can recover from
  /// a permanent-disconnect state (e.g. after 20 failed auto-reconnects).
  const reconnect = useCallback(() => {
    clearTimeout(reconnectTimer.current);
    retryCount.current = 0;
    setConnectionStatus('connecting');
    connect();
  }, [connect]);

  useEffect(() => {
    if (isTauri()) return; // In Tauri mode, chat uses IPC events, not WebSocket
    connect();
    return () => {
      clearTimeout(reconnectTimer.current);
      clearHeartbeat();
      wsRef.current?.close();
    };
  }, [connect]);

  return { sendMessage, sendApproval, sendInput, sendSelection, cancel, reconnect, connectionStatus };
}
