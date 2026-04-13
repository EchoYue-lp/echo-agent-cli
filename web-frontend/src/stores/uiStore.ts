import { create } from 'zustand';

type TabId = 'tools' | 'mcp' | 'skills' | 'memory' | 'config' | 'audit' | 'workflow' | 'permissions' | 'sessions' | 'sandbox' | 'compress' | 'extract';

type Theme = 'light' | 'dark';

interface UiState {
  leftSidebarOpen: boolean;
  rightPanelOpen: boolean;
  activeTab: TabId;
  theme: Theme;
  toggleLeftSidebar: () => void;
  toggleRightPanel: () => void;
  setActiveTab: (tab: TabId) => void;
  openRightPanel: (tab?: TabId) => void;
  toggleTheme: () => void;
  setTheme: (theme: Theme) => void;
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
  rightPanelOpen: false,
  activeTab: 'tools',
  theme: getInitialTheme(),

  toggleLeftSidebar: () => set((s) => ({ leftSidebarOpen: !s.leftSidebarOpen })),
  toggleRightPanel: () => set((s) => ({ rightPanelOpen: !s.rightPanelOpen })),
  setActiveTab: (tab) => set({ activeTab: tab, rightPanelOpen: true }),
  openRightPanel: (tab) => set({ rightPanelOpen: true, activeTab: tab ?? get().activeTab }),
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
}));

// Apply theme on load
if (typeof window !== 'undefined') {
  applyTheme(getInitialTheme());
}

export type { TabId, Theme };
