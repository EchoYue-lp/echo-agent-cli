// Tauri 流式聊天 Hook
//
// 使用 Tauri IPC events 进行流式聊天，替代 WebSocket

import { useEffect, useRef } from 'react';
import { useChatStore } from '../stores/chatStore';
import { isTauri, listenChatEvents, tauriChatStream, tauriCancelChat } from '../api/tauriClient';
import type { ChatEvent } from '../api/tauriClient';

export function useTauriChat() {
  const unlistenRef = useRef<(() => void) | null>(null);
  const cidRef = useRef<string | null>(null);

  useEffect(() => {
    if (!isTauri()) return;

    // Listen for chat events
    listenChatEvents((event: ChatEvent) => {
      const store = useChatStore.getState();

      switch (event.type) {
        case 'token': {
          const streamingMsg = store.messages.find((m) => m.isStreaming);
          if (streamingMsg && event.data) {
            if (store.isThinking) {
              store.appendThinking(streamingMsg.id, event.data);
            } else {
              store.appendToken(streamingMsg.id, event.data);
            }
          }
          break;
        }
        case 'think_start': {
          store.setThinking(true);
          const streamingMsg = store.messages.find((m) => m.isStreaming);
          if (streamingMsg) {
            store.startThinkingSegment(streamingMsg.id);
          }
          break;
        }
        case 'think_end': {
          store.setThinking(false);
          break;
        }
        case 'tool_call': {
          if (event.name) {
            store.setToolCall(event.name, event.args);
          }
          break;
        }
        case 'tool_result': {
          if (event.name) {
            store.completeToolCall(event.name, event.output || '', true);
          }
          break;
        }
        case 'tool_error': {
          if (event.name) {
            store.completeToolCall(event.name, event.error || '', false);
          }
          break;
        }
        case 'final_answer': {
          store.setStreaming(false);
          store.setThinking(false);
          break;
        }
        case 'cancelled': {
          store.markCancelled();
          store.setThinking(false);
          break;
        }
        case 'error': {
          store.setStreaming(false);
          store.setThinking(false);
          break;
        }
      }
    }).then((unlisten) => {
      unlistenRef.current = unlisten;
    });

    return () => {
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
    };
  }, []);

  const sendMessage = async (message: string) => {
    if (!isTauri()) return false;

    const store = useChatStore.getState();
    store.addUserMessage(message);
    const assistantId = store.startAssistantMessage();

    const conversationId = cidRef.current || `conv-${Date.now()}`;
    cidRef.current = conversationId;

    try {
      await tauriChatStream(message, conversationId);
    } catch (e) {
      console.error('Tauri chat stream error:', e);
      store.setStreaming(false);
    }

    return true;
  };

  const cancelChat = async () => {
    if (cidRef.current) {
      await tauriCancelChat(cidRef.current);
    }
  };

  return { sendMessage, cancelChat, isTauri: isTauri() };
}
