import { useRef, useCallback, useEffect, useState } from 'react';
import { useChatStore } from '../stores/chatStore';
import type { ClientMessage, ServerMessage, Attachment } from '../types/api';

export type ConnectionStatus = 'connected' | 'disconnected' | 'connecting';

const INITIAL_RECONNECT_MS = 1000;
const MAX_RECONNECT_MS = 30000;

export function useWebSocket() {
  const wsRef = useRef<WebSocket | null>(null);
  const assistantIdRef = useRef<string | null>(null);
  const inThinkingRef = useRef(false);
  const reconnectTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const retryCount = useRef(0);
  const [connectionStatus, setConnectionStatus] = useState<ConnectionStatus>('connecting');

  const getReconnectDelay = useCallback(() => {
    const delay = Math.min(INITIAL_RECONNECT_MS * Math.pow(2, retryCount.current), MAX_RECONNECT_MS);
    return delay;
  }, []);

  const connect = useCallback(() => {
    // Guard: skip if already open or connecting
    if (wsRef.current?.readyState === WebSocket.OPEN ||
        wsRef.current?.readyState === WebSocket.CONNECTING) return;

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
      retryCount.current = 0; // Reset backoff on successful connection
    };

    ws.onmessage = (ev) => {
      const msg: ServerMessage = JSON.parse(ev.data);
      const store = useChatStore.getState();

      switch (msg.type) {
        case 'token': {
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
          store.setToolCall(msg.name, msg.args);
          break;
        case 'tool_result':
          store.completeToolCall(msg.name, msg.result, msg.success);
          break;
        case 'final_answer': {
          if (assistantIdRef.current) {
            store.finalizeAssistantMessage(assistantIdRef.current, msg.data);
          }
          assistantIdRef.current = null;
          break;
        }
        case 'approval_request':
          store.setApprovalRequest({
            requestId: msg.request_id,
            toolName: msg.tool_name,
            args: msg.args,
            prompt: msg.prompt,
          });
          break;
        case 'input_request':
          store.setInputRequest({ requestId: msg.request_id, prompt: msg.prompt });
          break;
        case 'chart':
          store.addChartMessage(msg.spec);
          break;
        case 'error': {
          if (assistantIdRef.current) {
            store.finalizeAssistantMessage(assistantIdRef.current, `[Error] ${msg.message}`);
          }
          assistantIdRef.current = null;
          break;
        }
        case 'cancelled':
          assistantIdRef.current = null;
          break;
      }
    };

    ws.onclose = () => {
      const delay = getReconnectDelay();
      retryCount.current += 1;
      console.log(`[WS] disconnected, reconnecting in ${delay}ms (attempt ${retryCount.current})`);
      setConnectionStatus('disconnected');
      reconnectTimer.current = setTimeout(connect, delay);
    };

    ws.onerror = () => {
      console.error('[WS] error');
      setConnectionStatus('disconnected');
    };
  }, [getReconnectDelay]);

  const send = useCallback((msg: ClientMessage): boolean => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      wsRef.current.send(JSON.stringify(msg));
      return true;
    }
    return false;
  }, []);

  const sendMessage = useCallback((text: string, attachments?: Attachment[]) => {
    const store = useChatStore.getState();
    const displayAttachments = attachments?.map((a) => ({
      name: a.name,
      mime_type: a.mime_type,
      url: `data:${a.mime_type};base64,${a.data}`,
      size: a.size,
    }));
    store.addUserMessage(text || '(附件)', displayAttachments);

    if (!send({ type: 'message', data: text, attachments })) {
      // WebSocket not connected — don't leave UI stuck in streaming
      store.markCancelled();
    } else {
      assistantIdRef.current = store.startAssistantMessage();
    }
  }, [send]);

  const sendApproval = useCallback((requestId: string, approved: boolean, reason?: string) => {
    send({ type: 'approval_response', request_id: requestId, approved, reason });
    useChatStore.getState().setApprovalRequest(null);
  }, [send]);

  const sendInput = useCallback((requestId: string, text: string) => {
    send({ type: 'input_response', request_id: requestId, text });
    useChatStore.getState().setInputRequest(null);
  }, [send]);

  const cancel = useCallback(() => {
    useChatStore.getState().markCancelled();
    assistantIdRef.current = null;
    send({ type: 'cancel' });
  }, [send]);

  useEffect(() => {
    connect();
    return () => {
      clearTimeout(reconnectTimer.current);
      wsRef.current?.close();
    };
  }, [connect]);

  return { sendMessage, sendApproval, sendInput, cancel, connectionStatus };
}
