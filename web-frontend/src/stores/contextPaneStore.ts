import { create } from 'zustand';

export type SubagentPaneTarget = {
  kind: 'subagent';
  runId: string;
  subagentRunId: string;
};

export type ContextPaneTarget =
  | { kind: 'tasks' }
  | { kind: 'browser' }
  | { kind: 'files' }
  | SubagentPaneTarget;

interface ContextPaneState {
  target: ContextPaneTarget | null;
  returnTarget: SubagentPaneTarget | null;
  width: number;
  openTasks: () => void;
  openBrowser: () => void;
  openFiles: () => void;
  openSubagent: (runId: string, subagentRunId: string) => void;
  close: () => void;
  reset: () => void;
  setWidth: (width: number) => void;
}

function storageWidth(): number | null {
  if (typeof window === 'undefined') return null;
  try {
    const stored = Number.parseInt(localStorage.getItem('eko-context-pane-width') ?? '', 10);
    return Number.isFinite(stored) ? stored : null;
  } catch {
    return null;
  }
}

function initialWidth(): number {
  return boundContextPaneWidth(storageWidth() ?? 520);
}

export function boundContextPaneWidth(width: number): number {
  return Math.min(760, Math.max(380, width));
}

export function contextPaneWidthForViewport(
  preferredWidth: number,
  viewportWidth: number,
  leftSidebarOpen: boolean
): number {
  const bounded = boundContextPaneWidth(preferredWidth);
  if (viewportWidth < 1280) return Math.min(bounded, Math.floor(viewportWidth * 0.94));

  const leftSidebarWidth = leftSidebarOpen ? 264 : 0;
  const available = viewportWidth - leftSidebarWidth - 520;
  return Math.max(380, Math.min(bounded, available));
}

function contextualToolTarget(
  state: ContextPaneState,
  target: Extract<ContextPaneTarget, { kind: 'browser' | 'files' }>
): Partial<ContextPaneState> {
  const returnTarget = state.target?.kind === 'subagent' ? state.target : state.returnTarget;
  return { target, returnTarget };
}

export const useContextPaneStore = create<ContextPaneState>((set) => ({
  target: null,
  returnTarget: null,
  width: initialWidth(),
  openTasks: () => set({ target: { kind: 'tasks' }, returnTarget: null }),
  openBrowser: () => set((state) => contextualToolTarget(state, { kind: 'browser' })),
  openFiles: () => set((state) => contextualToolTarget(state, { kind: 'files' })),
  openSubagent: (runId, subagentRunId) =>
    set({ target: { kind: 'subagent', runId, subagentRunId }, returnTarget: null }),
  close: () =>
    set((state) =>
      state.returnTarget
        ? { target: state.returnTarget, returnTarget: null }
        : { target: null, returnTarget: null }
    ),
  reset: () => set({ target: null, returnTarget: null }),
  setWidth: (width) => {
    const bounded = boundContextPaneWidth(width);
    try {
      localStorage.setItem('eko-context-pane-width', String(bounded));
    } catch {
      // The in-memory preference still applies when storage is unavailable.
    }
    set({ width: bounded });
  },
}));
