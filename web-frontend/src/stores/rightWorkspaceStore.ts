import { create } from 'zustand';

export type RightWorkspaceTab =
  'tasks' | 'analysis' | 'research' | 'browser' | 'files' | 'automation';
export type AutomationView = 'workflows' | 'extract';

interface RightWorkspaceState {
  open: boolean;
  activeTab: RightWorkspaceTab;
  automationView: AutomationView;
  width: number;
  openWorkspace: () => void;
  openTasks: () => void;
  openAnalysis: () => void;
  openResearch: () => void;
  openBrowser: () => void;
  openFiles: () => void;
  openWorkflows: () => void;
  openExtract: () => void;
  close: () => void;
  setActiveTab: (tab: RightWorkspaceTab) => void;
  setAutomationView: (view: AutomationView) => void;
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
  automationView: 'workflows',
  width: initialWidth(),
  openWorkspace: () => set({ open: true }),
  openTasks: () => set({ open: true, activeTab: 'tasks' }),
  openAnalysis: () => set({ open: true, activeTab: 'analysis' }),
  openResearch: () => set({ open: true, activeTab: 'research' }),
  openBrowser: () => set({ open: true, activeTab: 'browser' }),
  openFiles: () => set({ open: true, activeTab: 'files' }),
  openWorkflows: () => set({ open: true, activeTab: 'automation', automationView: 'workflows' }),
  openExtract: () => set({ open: true, activeTab: 'automation', automationView: 'extract' }),
  close: () => set({ open: false }),
  setActiveTab: (activeTab) => set({ activeTab }),
  setAutomationView: (automationView) => set({ automationView }),
  setWidth: (width) => {
    const bounded = boundRightWorkspaceWidth(width);
    localStorage.setItem('eko-right-workspace-width', String(bounded));
    set({ width: bounded });
  },
}));
