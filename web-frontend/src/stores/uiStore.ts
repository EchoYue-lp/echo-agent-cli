import { create } from 'zustand';

export type SettingsTabId =
  | 'overview'
  | 'tools'
  | 'mcp'
  | 'skills'
  | 'memory'
  | 'config'
  | 'providers'
  | 'sessions'
  | 'audit'
  | 'sandbox'
  | 'observability'
  | 'scratchpad'
  | 'decisions'
  | 'compress'
  | 'evolution'
  | 'plugins'
  | 'scheduler'
  | 'worktree';

type Theme = 'light' | 'dark';

interface UiState {
  leftSidebarOpen: boolean;
  settingsOpen: boolean;
  activeSettingsTab: SettingsTabId;
  theme: Theme;
  terminalOpen: boolean;
  toggleLeftSidebar: () => void;
  openSettings: () => void;
  closeSettings: () => void;
  setActiveSettingsTab: (tab: SettingsTabId) => void;
  toggleTheme: () => void;
  setTheme: (theme: Theme) => void;
  openTerminal: () => void;
  closeTerminal: () => void;
  toggleTerminal: () => void;
}

function getInitialTheme(): Theme {
  if (typeof window === 'undefined') return 'light';
  const stored = localStorage.getItem('echo-theme') as Theme | null;
  if (stored === 'dark' || stored === 'light') return stored;
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

function applyTheme(theme: Theme) {
  document.documentElement.classList.toggle('dark', theme === 'dark');
}

export const useUiStore = create<UiState>((set, get) => ({
  leftSidebarOpen: typeof window !== 'undefined' && window.innerWidth >= 768,
  settingsOpen: false,
  activeSettingsTab: 'overview',
  theme: getInitialTheme(),
  terminalOpen: false,

  toggleLeftSidebar: () => set((s) => ({ leftSidebarOpen: !s.leftSidebarOpen })),
  openSettings: () => set({ settingsOpen: true }),
  closeSettings: () => set({ settingsOpen: false }),
  setActiveSettingsTab: (tab) => set({ activeSettingsTab: tab, settingsOpen: true }),
  toggleTheme: () => {
    const next = get().theme === 'dark' ? 'light' : 'dark';
    localStorage.setItem('echo-theme', next);
    applyTheme(next);
    set({ theme: next });
  },
  setTheme: (theme) => {
    localStorage.setItem('echo-theme', theme);
    applyTheme(theme);
    set({ theme });
  },
  openTerminal: () => set({ terminalOpen: true }),
  closeTerminal: () => set({ terminalOpen: false }),
  toggleTerminal: () => set((s) => ({ terminalOpen: !s.terminalOpen })),
}));

// Apply theme on load
if (typeof window !== 'undefined') {
  applyTheme(getInitialTheme());
}
