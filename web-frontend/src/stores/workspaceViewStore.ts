import { create } from 'zustand';

export type PrimaryWorkspaceView = 'chat' | 'analysis' | 'research' | 'workflow' | 'extract';

interface WorkspaceViewState {
  activeView: PrimaryWorkspaceView;
  open: (view: PrimaryWorkspaceView) => void;
  openChat: () => void;
}

export const useWorkspaceViewStore = create<WorkspaceViewState>((set) => ({
  activeView: 'chat',
  open: (activeView) => set({ activeView }),
  openChat: () => set({ activeView: 'chat' }),
}));
