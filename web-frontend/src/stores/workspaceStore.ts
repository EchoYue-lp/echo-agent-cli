import { create } from 'zustand';
import { workspaceApi, type Workspace } from '../api/endpoints';
import { useChatStore } from './chatStore';
import { useConversationStore } from './conversationStore';
import { useFileStore } from './fileStore';
import { useSubagentRunStore } from './subagentRunStore';
import { useTaskRuntimeStore } from './taskRuntimeStore';
import { useToastStore } from './toastStore';
import { useToolExecutionStore } from './toolExecutionStore';
import { useBrowserStore } from './browserStore';
import { GLOBAL_WORKSPACE_ID } from '../lib/viewAddress';

let workspaceGeneration = 0;

function detachVisibleWorkspace(workspaceId: string): void {
  useChatStore.getState().clearMessages();
  useTaskRuntimeStore.getState().reset();
  useToolExecutionStore.getState().clear();
  useSubagentRunStore.getState().clear();
  useBrowserStore.getState().clearWorkspace(workspaceId);
  useConversationStore.getState().detachForWorkspace(workspaceId);
}

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
    const generation = workspaceGeneration + 1;
    workspaceGeneration = generation;
    set({ isLoading: true });
    try {
      const [listRes, currentRes] = await Promise.all([
        workspaceApi.list(),
        workspaceApi.current().catch(() => ({ workspace: null, active: false })),
      ]);
      if (generation !== workspaceGeneration) return;
      set({
        workspaces: listRes.workspaces || [],
        current: currentRes.workspace || null,
        isLoading: false,
      });
    } catch (e) {
      console.error('Failed to load workspaces:', e);
      if (generation === workspaceGeneration) set({ isLoading: false });
    }
  },

  switchTo: async (id: string) => {
    const generation = workspaceGeneration + 1;
    workspaceGeneration = generation;
    const previousWorkspaceId = get().current?.id ?? GLOBAL_WORKSPACE_ID;
    detachVisibleWorkspace(previousWorkspaceId);
    set({ isLoading: true });
    try {
      if (import.meta.env.DEV) console.debug('[workspaceStore] switchTo:', id);
      const res = await workspaceApi.switch(id);
      if (generation !== workspaceGeneration) return;
      if (import.meta.env.DEV)
        console.debug(
          '[workspaceStore] switch API returned:',
          res.workspace?.name,
          'debug_conv_count:',
          (res as any).debug_conversation_count
        );
      set({ current: res.workspace, isLoading: false });
      showTransitionWarning(res.transition);
      const fileStore = useFileStore.getState();
      fileStore.markWorkspaceChanged();
      void fileStore.loadTree(4);
      void fileStore.loadChanges();

      // Reload conversations from the new workspace's store
      await useConversationStore.getState().init(res.workspace.id);
      if (generation !== workspaceGeneration) return;
      if (import.meta.env.DEV)
        console.debug(
          '[workspaceStore] conversations loaded:',
          useConversationStore.getState().conversations.length
        );
    } catch (e) {
      console.error('[workspaceStore] Failed to switch workspace:', e);
      if (generation === workspaceGeneration) {
        detachVisibleWorkspace(previousWorkspaceId);
        await useConversationStore.getState().init(previousWorkspaceId);
        set({ isLoading: false });
      }
      throw e;
    }
  },

  createAndSwitch: async (name: string, kind?: string, root?: string) => {
    const generation = workspaceGeneration + 1;
    workspaceGeneration = generation;
    const previousWorkspaceId = get().current?.id ?? GLOBAL_WORKSPACE_ID;
    detachVisibleWorkspace(previousWorkspaceId);
    set({ isLoading: true });
    const res = await workspaceApi.createAndSwitch(name, kind, root);
    if (!res.success) {
      if (generation === workspaceGeneration) {
        await useConversationStore.getState().init(previousWorkspaceId);
        set({ isLoading: false });
      }
      const prefix = res.created ? '工作区已创建，但进入失败' : '无法创建并进入工作区';
      throw new Error(`${prefix}：${res.error}`);
    }
    if (generation !== workspaceGeneration) return res.workspace;
    const ws = res.workspace;
    set({ current: ws, isLoading: false });
    showTransitionWarning(res.transition);
    const fileStore = useFileStore.getState();
    fileStore.markWorkspaceChanged();
    void fileStore.loadTree(4);
    void fileStore.loadChanges();
    await useConversationStore.getState().init(ws.id);
    return ws;
  },

  delete: async (id: string) => {
    await workspaceApi.delete(id);
    const { current } = get();
    if (current?.id === id) {
      detachVisibleWorkspace(id);
      set({ current: null });
      useFileStore.getState().markWorkspaceChanged();
    }
    await get().init();
    if (current?.id === id) {
      await useConversationStore.getState().init(GLOBAL_WORKSPACE_ID);
    }
  },

  exit: async () => {
    const generation = workspaceGeneration + 1;
    workspaceGeneration = generation;
    detachVisibleWorkspace(get().current?.id ?? GLOBAL_WORKSPACE_ID);
    const res = await workspaceApi.exit();
    if (generation !== workspaceGeneration) return;
    set({ current: null });
    showTransitionWarning(res.transition);
    useFileStore.getState().markWorkspaceChanged();
    await useConversationStore.getState().init(GLOBAL_WORKSPACE_ID);
  },
}));
