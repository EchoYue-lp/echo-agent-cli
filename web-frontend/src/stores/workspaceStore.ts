import { create } from 'zustand';
import { sessionApi, workspaceApi, type Workspace } from '../api/endpoints';
import { useChatStore } from './chatStore';
import { useConversationStore } from './conversationStore';
import { useFileStore } from './fileStore';
import { useToastStore } from './toastStore';

function showTransitionWarning(
  transition: Awaited<ReturnType<typeof workspaceApi.switch>>['transition']
) {
  if (transition.status !== 'degraded') return;
  const subsystems = transition.degraded_subsystems
    .map((subsystem) => `${subsystem.subsystem}: ${subsystem.error}`)
    .join('; ');
  useToastStore
    .getState()
    .addToast('warning', `工作区已切换，但部分子系统降级：${subsystems}`, 8000);
}

interface WorkspaceState {
  /** All workspaces */
  workspaces: Workspace[];
  /** Currently active workspace */
  current: Workspace | null;
  /** Whether workspace list is loading */
  isLoading: boolean;

  /** Initialize: load workspaces and current */
  init: () => Promise<void>;
  /** Switch to a workspace */
  switchTo: (id: string) => Promise<void>;
  /** Create and switch to a new workspace */
  createAndSwitch: (name: string, kind?: string, root?: string) => Promise<Workspace>;
  /** Delete a workspace */
  delete: (id: string) => Promise<void>;
  /** Exit workspace (back to global mode) */
  exit: () => Promise<void>;
}

export const useWorkspaceStore = create<WorkspaceState>((set, get) => ({
  workspaces: [],
  current: null,
  isLoading: false,

  init: async () => {
    set({ isLoading: true });
    try {
      const [listRes, currentRes] = await Promise.all([
        workspaceApi.list(),
        workspaceApi.current().catch(() => ({ workspace: null, active: false })),
      ]);
      set({
        workspaces: listRes.workspaces || [],
        current: currentRes.workspace || null,
        isLoading: false,
      });
    } catch (e) {
      console.error('Failed to load workspaces:', e);
      set({ isLoading: false });
    }
  },

  switchTo: async (id: string) => {
    try {
      if (import.meta.env.DEV) console.debug('[workspaceStore] switchTo:', id);
      const res = await workspaceApi.switch(id);
      if (import.meta.env.DEV)
        console.debug(
          '[workspaceStore] switch API returned:',
          res.workspace?.name,
          'debug_conv_count:',
          (res as any).debug_conversation_count
        );
      set({ current: res.workspace });
      showTransitionWarning(res.transition);
      const fileStore = useFileStore.getState();
      fileStore.markWorkspaceChanged();
      void fileStore.loadTree(4);
      void fileStore.loadChanges();

      // Clear current chat
      useChatStore.getState().clearMessages();

      // Reset agent session (best-effort, must not block)
      try {
        await sessionApi.reset();
      } catch (e) {
        console.warn('[workspaceStore] session reset failed (non-fatal):', e);
      }

      // Reload conversations from the new workspace's store
      useConversationStore.setState({ activeId: null });
      await useConversationStore.getState().init();
      if (import.meta.env.DEV)
        console.debug(
          '[workspaceStore] conversations loaded:',
          useConversationStore.getState().conversations.length
        );
    } catch (e) {
      console.error('[workspaceStore] Failed to switch workspace:', e);
      throw e;
    }
  },

  createAndSwitch: async (name: string, kind?: string, root?: string) => {
    const res = await workspaceApi.create(name, kind, root);
    const ws = res.workspace;
    // Refresh list
    await get().init();
    // Switch to it
    await get().switchTo(ws.id);
    return ws;
  },

  delete: async (id: string) => {
    await workspaceApi.delete(id);
    const { current } = get();
    if (current?.id === id) {
      set({ current: null });
    }
    await get().init();
  },

  exit: async () => {
    const res = await workspaceApi.exit();
    set({ current: null });
    showTransitionWarning(res.transition);
    useFileStore.getState().markWorkspaceChanged();
  },
}));
