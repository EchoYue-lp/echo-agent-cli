import { create } from 'zustand';
import { workspaceApi, type Workspace } from '../api/endpoints';

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
  exit: () => void;
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
      const res = await workspaceApi.switch(id);
      set({ current: res.workspace });
      // Re-init conversation list for this workspace
      const { useConversationStore } = await import('./conversationStore');
      useConversationStore.getState().init();
    } catch (e) {
      console.error('Failed to switch workspace:', e);
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

  exit: () => {
    set({ current: null });
  },
}));
