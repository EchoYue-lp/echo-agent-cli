import { useEffect, useState } from 'react';
import { conversationApi } from '../api/endpoints';
import { useChatStore } from '../stores/chatStore';
import { useConversationStore } from '../stores/conversationStore';
import { useWorkspaceStore } from '../stores/workspaceStore';
import { workspaceIdForView } from '../lib/viewAddress';
import type { ConversationUsageSnapshot } from '../types/api';

/** 只查询原 journal 派生累计，不从可见消息/前端尾窗重新记账。 */
export function useConversationUsage() {
  const workspace = useWorkspaceStore((state) => workspaceIdForView(state.current?.id));
  const generation = useWorkspaceStore(
    (state) => state.current?.product_data_generation ?? 'global'
  );
  const conversation = useConversationStore((state) => state.activeId);
  const streaming = useChatStore((state) => state.isStreaming);
  const [view, setView] = useState<{
    key: string;
    value: ConversationUsageSnapshot | null;
    error: string | null;
  } | null>(null);
  const key = JSON.stringify([workspace, generation, conversation]);
  useEffect(() => {
    if (!conversation) return;
    let cancelled = false;
    let busy = false;
    const refresh = async () => {
      if (busy) return;
      busy = true;
      try {
        const value = await conversationApi.usage(workspace, conversation);
        if (
          !cancelled &&
          value.workspace_id === workspace &&
          value.conversation_id === conversation
        ) {
          setView((previous) => {
            // 同代原日志的旧响应不能覆盖更新游标；代际切换由 effect fence 隔离。
            if (
              previous?.key === key &&
              previous.value &&
              JSON.stringify(previous.value.journal_identity) ===
                JSON.stringify(value.journal_identity) &&
              previous.value.through_sequence > value.through_sequence
            )
              return previous;
            return { key, value, error: null };
          });
        }
      } catch (error) {
        if (!cancelled)
          setView((previous) => ({
            key,
            value: previous?.key === key ? previous.value : null,
            error: error instanceof Error ? error.message : '原日志统计暂不可读',
          }));
      } finally {
        busy = false;
      }
    };
    void refresh();
    // 长流固定节奏刷新，避免每个 token 查询或 debounce 长期饥饿；结束再读取一次。
    const timer = streaming ? window.setInterval(() => void refresh(), 1500) : undefined;
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearInterval(timer);
    };
  }, [workspace, generation, conversation, streaming, key]);
  return view?.key === key ? view : { value: null, error: null };
}
