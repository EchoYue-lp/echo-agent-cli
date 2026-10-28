import { useEffect } from 'react';
import { conversationApi } from '../api/endpoints';
import { useChatStore } from '../stores/chatStore';
import { useConversationStore } from '../stores/conversationStore';
import { useWorkspaceStore } from '../stores/workspaceStore';
import { productDataScope, sameProductDataScope } from '../lib/productDataScope';

/** Poll the original durable inbox read projection. No delivery wake, new store or model loop. */
export function useAgentCommunicationInputs(
  workspaceId: string,
  conversationId: string | null
): string {
  const workspaceGeneration = useWorkspaceStore(
    (state) => productDataScope(state.current).workspaceGeneration
  );
  const epoch = useConversationStore((state) => state.newConversationEpoch);
  const binding = JSON.stringify([workspaceId, workspaceGeneration, conversationId, epoch]);
  useEffect(() => {
    if (!conversationId) return;
    let current = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const scope = productDataScope(useWorkspaceStore.getState().current);

    const refresh = async () => {
      try {
        const records = await conversationApi.agentInputs(scope, conversationId);
        if (
          current &&
          sameProductDataScope(scope, productDataScope(useWorkspaceStore.getState().current)) &&
          useConversationStore.getState().activeId === conversationId &&
          useConversationStore.getState().newConversationEpoch === epoch
        )
          useChatStore.getState().observeAgentInputs(workspaceId, conversationId, records, binding);
      } catch (error) {
        // A failed read never erases messages or masquerades as an empty/successful inbox.
        if (
          current &&
          sameProductDataScope(scope, productDataScope(useWorkspaceStore.getState().current)) &&
          useConversationStore.getState().activeId === conversationId &&
          useConversationStore.getState().newConversationEpoch === epoch
        )
          useChatStore
            .getState()
            .noteAgentInputReadError(
              binding,
              error instanceof Error ? error.message : '跨会话消息读取失败'
            );
      } finally {
        if (current) timer = setTimeout(() => void refresh(), 1000);
      }
    };
    void refresh();
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [workspaceId, workspaceGeneration, conversationId, epoch, binding]);
  return binding;
}
