import { useRef, useCallback, useEffect, useState } from 'react';
import { useChatStore } from '../stores/chatStore';
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
  const inThinkingRef = useRef(false);
  const isCancelledRef = useRef(false);
  const reconnectTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const streamTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const heartbeatTimer = useRef<ReturnType<typeof setInterval>>(undefined);
  const heartbeatTimeout = useRef<ReturnType<typeof setTimeout>>(undefined);
  const messageQueue = useRef<ClientMessage[]>([]);
  const retryCount = useRef(0);
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
            wsRef.current?.close();
          }, HEARTBEAT_TIMEOUT_MS);
        }
      }, HEARTBEAT_INTERVAL_MS);

      // Flush queued messages
      const queued = messageQueue.current;
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

      switch (msg.type) {
        case 'token': {
          clearStreamTimer();
          if (isCancelledRef.current) break;
          if (!assistantIdRef.current) {
            assistantIdRef.current = store.startAssistantMessage();
          }
          if (inThinkingRef.current) {
            store.appendThinking(assistantIdRef.current, msg.data);
          } else {
            store.appendToken(assistantIdRef.current, msg.data);
          }
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
          store.setToolCall(msg.name, msg.args);
          break;
        case 'tool_result':
          if (isCancelledRef.current) break;
          store.completeToolCall(msg.name, msg.result, msg.success);
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
          assistantIdRef.current = null;
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
          clearStreamTimer();
          if (!isCancelledRef.current && assistantIdRef.current) {
            store.finalizeAssistantMessage(assistantIdRef.current, `[Error] ${msg.message}`);
          }
          assistantIdRef.current = null;
          isCancelledRef.current = false;
          break;
        }
        case 'cancelled':
          clearStreamTimer();
          assistantIdRef.current = null;
          isCancelledRef.current = false;
          break;
      }
    };

    ws.onclose = () => {
      clearHeartbeat();
      retryCount.current += 1;

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
    // Queue message for when reconnection succeeds
    if (msg.type !== 'ping' && msg.type !== 'cancel') {
      messageQueue.current.push(msg);
    }
    return false;
  }, []);

  const clearStreamTimer = () => {
    if (streamTimer.current) {
      clearTimeout(streamTimer.current);
      streamTimer.current = undefined;
    }
  };

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

      if (!send({ type: 'message', data: text, attachments })) {
        store.markCancelled();
      } else {
        isCancelledRef.current = false;
        assistantIdRef.current = store.startAssistantMessage();
        // 60s streaming timeout: if no response, cancel to prevent stuck UI
        clearStreamTimer();
        streamTimer.current = setTimeout(() => {
          if (useChatStore.getState().isStreaming) {
            useChatStore.getState().markCancelled();
          }
        }, 60_000);
      }
    },
    [send]
  );

  const sendApproval = useCallback(
    (requestId: string, approved: boolean, reason?: string) => {
      send({ type: 'approval_response', request_id: requestId, approved, reason });
      useChatStore.getState().setApprovalRequest(null);
    },
    [send]
  );

  const sendInput = useCallback(
    (requestId: string, text: string) => {
      send({ type: 'input_response', request_id: requestId, text });
      useChatStore.getState().setInputRequest(null);
    },
    [send]
  );

  const cancel = useCallback(() => {
    useChatStore.getState().markCancelled();
    assistantIdRef.current = null;
    isCancelledRef.current = true;
    send({ type: 'cancel' });
  }, [send]);

  useEffect(() => {
    if (isTauri()) return; // In Tauri mode, chat uses IPC events, not WebSocket
    connect();
    return () => {
      clearTimeout(reconnectTimer.current);
      clearStreamTimer();
      clearHeartbeat();
      wsRef.current?.close();
    };
  }, [connect]);

  return { sendMessage, sendApproval, sendInput, cancel, connectionStatus };
}
