import { useRef, useCallback, useEffect, useState } from 'react';
import { useChatStore } from '../stores/chatStore';
import type { ClientMessage, ServerMessage } from '../types/api';

export type ConnectionStatus = 'connected' | 'disconnected' | 'connecting';

export function useWebSocket() {
  const wsRef = useRef<WebSocket | null>(null);
  const assistantIdRef = useRef<string | null>(null);
  const reconnectTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const [connectionStatus, setConnectionStatus] = useState<ConnectionStatus>('connecting');

  const connect = useCallback(() => {
    if (wsRef.current?.readyState === WebSocket.OPEN) return;

    const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const ws = new WebSocket(`${protocol}//${location.host}/ws/chat`);
    wsRef.current = ws;
    setConnectionStatus('connecting');
    ws.onopen = () => {
      console.log('[WS] connected');
      setConnectionStatus('connected');
    };

    ws.onmessage = (ev) => {
      const msg: ServerMessage = JSON.parse(ev.data);
      const store = useChatStore.getState();

      switch (msg.type) {
        case 'token': {
          if (!assistantIdRef.current) {
            assistantIdRef.current = store.startAssistantMessage();
          }
          store.appendToken(assistantIdRef.current, msg.data);
          break;
        }
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
      console.log('[WS] disconnected, reconnecting in 2s...');
      setConnectionStatus('disconnected');
      reconnectTimer.current = setTimeout(connect, 2000);
    };

    ws.onerror = () => {
      console.error('[WS] error');
      setConnectionStatus('disconnected');
    };
  }, []);

  const send = useCallback((msg: ClientMessage) => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      wsRef.current.send(JSON.stringify(msg));
    }
  }, []);

  const sendMessage = useCallback((text: string) => {
    const store = useChatStore.getState();
    store.addUserMessage(text);
    send({ type: 'message', data: text });
    assistantIdRef.current = store.startAssistantMessage();
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
