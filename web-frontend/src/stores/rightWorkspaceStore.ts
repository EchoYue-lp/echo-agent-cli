import { create } from 'zustand';

export type RightWorkspaceTab = 'tasks' | 'analysis' | 'browser' | 'files';

interface RightWorkspaceState {
  open: boolean;
  activeTab: RightWorkspaceTab;
  width: number;
  openWorkspace: () => void;
  openTasks: () => void;
  openAnalysis: () => void;
  openBrowser: () => void;
  openFiles: () => void;
  close: () => void;
  setActiveTab: (tab: RightWorkspaceTab) => void;
  setWidth: (width: number) => void;
}

function initialWidth(): number {
  if (typeof window === 'undefined') return 520;
  const stored = Number.parseInt(localStorage.getItem('eko-right-workspace-width') ?? '', 10);
  return Number.isFinite(stored) ? boundRightWorkspaceWidth(stored) : 520;
}

export function boundRightWorkspaceWidth(width: number): number {
  return Math.min(760, Math.max(380, width));
}

export function rightWorkspaceWidthForViewport(
  preferredWidth: number,
  viewportWidth: number,
  leftSidebarOpen: boolean
): number {
  const bounded = boundRightWorkspaceWidth(preferredWidth);
  if (viewportWidth < 1280) return Math.min(bounded, Math.floor(viewportWidth * 0.94));

  const leftSidebarWidth = leftSidebarOpen ? 272 : 0;
  const available = viewportWidth - leftSidebarWidth - 520;
  return Math.max(380, Math.min(bounded, available));
}

export const useRightWorkspaceStore = create<RightWorkspaceState>((set) => ({
  open: false,
  activeTab: 'tasks',
  width: initialWidth(),
  openWorkspace: () => set({ open: true }),
  openTasks: () => set({ open: true, activeTab: 'tasks' }),
  openAnalysis: () => set({ open: true, activeTab: 'analysis' }),
  openBrowser: () => set({ open: true, activeTab: 'browser' }),
  openFiles: () => set({ open: true, activeTab: 'files' }),
  close: () => set({ open: false }),
  setActiveTab: (activeTab) => set({ activeTab }),
  setWidth: (width) => {
    const bounded = boundRightWorkspaceWidth(width);
    localStorage.setItem('eko-right-workspace-width', String(bounded));
    set({ width: bounded });
  },
}));
