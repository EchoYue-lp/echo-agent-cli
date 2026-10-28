import { create } from 'zustand';
import { workspaceApi, type Workspace, type ProjectSource } from '../api/endpoints';
import { useChatStore } from './chatStore';
import { useConversationStore } from './conversationStore';
import { useFileStore } from './fileStore';
import { useSubagentRunStore } from './subagentRunStore';
import { useTaskRuntimeStore } from './taskRuntimeStore';
import { useToastStore } from './toastStore';
import { useToolExecutionStore } from './toolExecutionStore';
import { useBrowserStore } from './browserStore';
import { GLOBAL_WORKSPACE_ID } from '../lib/viewAddress';
import { productDataScope, sameProductDataScope } from '../lib/productDataScope';

let workspaceGeneration = 0;
let workspaceListGeneration = 0;

function includeWorkspace(workspaces: Workspace[], workspace: Workspace): Workspace[] {
  // 创建/打开回执已来自原 registry；同步侧栏列表并替换同 ID 的旧 generation。
  return [workspace, ...workspaces.filter((item) => item.id !== workspace.id)];
}

function detachVisibleWorkspace(workspaceId: string): void {
  useChatStore.getState().clearMessages();
  useTaskRuntimeStore.getState().reset();
  useToolExecutionStore.getState().clear();
  useSubagentRunStore.getState().clear();
  useBrowserStore.getState().clearWorkspace(workspaceId);
  useConversationStore.getState().detachForWorkspace(workspaceId);
}

async function recoverVisibleWorkspace(generation: number): Promise<void> {
  const response = await workspaceApi.current();
  if (generation !== workspaceGeneration) return;
  const workspace = response.workspace;
  useWorkspaceStore.setState({ current: workspace, isLoading: false });
  useFileStore.getState().bindScope(productDataScope(workspace));
  await useConversationStore.getState().init(workspace?.id ?? GLOBAL_WORKSPACE_ID);
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
  switchTo: (id: string, connectionId?: string, expectedGeneration?: string) => Promise<void>;
  /** Refresh only canonical registry rows without moving current selection. */
  refreshList: () => Promise<void>;
  /** Create and switch to a new workspace */
  createAndSwitch: (
    name: string,
    kind?: string,
    root?: string,
    source?: ProjectSource
  ) => Promise<Workspace>;
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
    const listGeneration = ++workspaceListGeneration;
    set({ isLoading: true });
    try {
      const [listRes, currentRes] = await Promise.all([
        workspaceApi.list(),
        workspaceApi.current().catch(() => ({ workspace: null, active: false })),
      ]);
      if (generation !== workspaceGeneration) return;
      set((state) => ({
        workspaces:
          listGeneration === workspaceListGeneration ? listRes.workspaces || [] : state.workspaces,
        current: currentRes.workspace || null,
        isLoading: false,
      }));
      useFileStore.getState().bindScope(productDataScope(currentRes.workspace));
    } catch (e) {
      console.error('Failed to load workspaces:', e);
      if (generation === workspaceGeneration) set({ isLoading: false });
    }
  },

  refreshList: async () => {
    const sourceGeneration = workspaceGeneration;
    const listGeneration = ++workspaceListGeneration;
    const result = await workspaceApi.list();
    if (sourceGeneration === workspaceGeneration && listGeneration === workspaceListGeneration)
      set({ workspaces: result.workspaces ?? [] });
  },

  switchTo: async (id: string, connectionId?: string, expectedGeneration?: string) => {
    const generation = workspaceGeneration + 1;
    workspaceGeneration = generation;
    const previousScope = productDataScope(get().current);
    const previousWorkspaceId = get().current?.id ?? GLOBAL_WORKSPACE_ID;
    const fenced = expectedGeneration !== undefined;
    // Fenced Fork keeps the source projection until native CAS accepts this generation.
    if (!fenced) detachVisibleWorkspace(previousWorkspaceId);
    set({ isLoading: true });
    try {
      if (import.meta.env.DEV) console.debug('[workspaceStore] switchTo:', id);
      const res = fenced
        ? await workspaceApi.switch(id, connectionId, expectedGeneration)
        : connectionId
          ? await workspaceApi.switch(id, connectionId)
          : await workspaceApi.switch(id);
      if (generation !== workspaceGeneration) return;
      if (import.meta.env.DEV)
        console.debug(
          '[workspaceStore] switch API returned:',
          res.workspace?.name,
          'debug_conv_count:',
          (res as any).debug_conversation_count
        );
      if (fenced) {
        if (res.workspace.product_data_generation !== expectedGeneration)
          throw new Error('目标工作空间代际与切换回执不一致');
        detachVisibleWorkspace(previousWorkspaceId);
      }
      set((state) => ({
        current: res.workspace,
        isLoading: false,
        workspaces: [
          res.workspace,
          ...state.workspaces.filter((item) => item.id !== res.workspace.id),
        ],
      }));
      showTransitionWarning(res.transition);
      const fileStore = useFileStore.getState();
      fileStore.bindScope(productDataScope(res.workspace));
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
        if (fenced) {
          let sourceStillCurrent = false;
          try {
            const actual = await workspaceApi.current();
            sourceStillCurrent = sameProductDataScope(
              previousScope,
              productDataScope(actual.workspace)
            );
          } catch (inspectionError) {
            console.error('Failed to inspect workspace after fenced switch:', inspectionError);
          }
          if (generation !== workspaceGeneration) throw e;
          if (sourceStillCurrent) {
            set({ isLoading: false });
            throw e;
          }
        }
        detachVisibleWorkspace(previousWorkspaceId);
        try {
          await recoverVisibleWorkspace(generation);
        } catch (recoveryError) {
          console.error('Failed to recover workspace focus:', recoveryError);
        } finally {
          if (generation === workspaceGeneration) set({ isLoading: false });
        }
      }
      throw e;
    }
  },

  createAndSwitch: async (name: string, kind?: string, root?: string, source?: ProjectSource) => {
    const generation = workspaceGeneration + 1;
    workspaceGeneration = generation;
    const previousWorkspaceId = get().current?.id ?? GLOBAL_WORKSPACE_ID;
    detachVisibleWorkspace(previousWorkspaceId);
    set({ isLoading: true });
    let res: Awaited<ReturnType<typeof workspaceApi.createAndSwitch>>;
    try {
      res = await workspaceApi.createAndSwitch(name, kind, root, source);
    } catch (error) {
      if (generation === workspaceGeneration) {
        try {
          await recoverVisibleWorkspace(generation);
        } catch (recoveryError) {
          console.error('Failed to recover workspace focus:', recoveryError);
        } finally {
          if (generation === workspaceGeneration) set({ isLoading: false });
        }
      }
      throw error;
    }
    if (!res.success) {
      if (generation === workspaceGeneration) {
        // 创建已落盘但进入失败时，保留列表事实，不把它投影成当前工作空间。
        set((state) => ({ workspaces: includeWorkspace(state.workspaces, res.workspace) }));
        await useConversationStore.getState().init(previousWorkspaceId);
        if (generation === workspaceGeneration) set({ isLoading: false });
      }
      const prefix = res.created ? '工作区已创建，但进入失败' : '无法创建并进入工作区';
      throw new Error(`${prefix}：${res.error}`);
    }
    if (generation !== workspaceGeneration) return res.workspace;
    const ws = res.workspace;
    set((state) => ({
      current: ws,
      workspaces: includeWorkspace(state.workspaces, ws),
      isLoading: false,
    }));
    showTransitionWarning(res.transition);
    const fileStore = useFileStore.getState();
    fileStore.bindScope(productDataScope(ws));
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
      useFileStore.getState().bindScope(productDataScope(null));
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
    set({ isLoading: true });
    try {
      const res = await workspaceApi.exit();
      if (generation !== workspaceGeneration) return;
      set({ current: null });
      showTransitionWarning(res.transition);
      useFileStore.getState().bindScope(productDataScope(null));
      await useConversationStore.getState().init(GLOBAL_WORKSPACE_ID);
    } finally {
      // The existing transition owns its busy projection through success or failure.
      // A superseded exit must not clear a newer transition's loading state.
      if (generation === workspaceGeneration) set({ isLoading: false });
    }
  },
}));
