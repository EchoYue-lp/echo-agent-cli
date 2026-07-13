import { create } from 'zustand';

export type RightWorkspaceTab = 'tasks' | 'preview';
export type PreviewTab = 'browser' | 'files';

interface RightWorkspaceState {
  open: boolean;
  activeTab: RightWorkspaceTab;
  previewTab: PreviewTab;
  width: number;
  openTasks: () => void;
  openBrowser: () => void;
  openFiles: () => void;
  close: () => void;
  setActiveTab: (tab: RightWorkspaceTab) => void;
  setPreviewTab: (tab: PreviewTab) => void;
  setWidth: (width: number) => void;
}

function initialWidth(): number {
  if (typeof window === 'undefined') return 560;
  const stored = Number.parseInt(localStorage.getItem('eko-right-workspace-width') ?? '', 10);
  return Number.isFinite(stored) ? boundRightWorkspaceWidth(stored) : 560;
}

export function boundRightWorkspaceWidth(width: number): number {
  return Math.min(760, Math.max(380, width));
}

export const useRightWorkspaceStore = create<RightWorkspaceState>((set) => ({
  open: false,
  activeTab: 'tasks',
  previewTab: 'browser',
  width: initialWidth(),
  openTasks: () => set({ open: true, activeTab: 'tasks' }),
  openBrowser: () => set({ open: true, activeTab: 'preview', previewTab: 'browser' }),
  openFiles: () => set({ open: true, activeTab: 'preview', previewTab: 'files' }),
  close: () => set({ open: false }),
  setActiveTab: (activeTab) => set({ activeTab }),
  setPreviewTab: (previewTab) => set({ activeTab: 'preview', previewTab }),
  setWidth: (width) => {
    const bounded = boundRightWorkspaceWidth(width);
    localStorage.setItem('eko-right-workspace-width', String(bounded));
    set({ width: bounded });
  },
}));
