import { create } from 'zustand';
import { pluginApi, type PluginThemeDefinition } from '../api/endpoints';

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
  | 'compress'
  | 'evolution'
  | 'plugins'
  | 'hooks'
  | 'scheduler'
  | 'worktree';

type Theme = 'light' | 'dark';

interface UiState {
  leftSidebarOpen: boolean;
  settingsOpen: boolean;
  activeSettingsTab: SettingsTabId;
  theme: Theme;
  activePluginTheme: string | null;
  terminalOpen: boolean;
  toggleLeftSidebar: () => void;
  openSettings: () => void;
  closeSettings: () => void;
  setActiveSettingsTab: (tab: SettingsTabId) => void;
  toggleTheme: () => Promise<void>;
  setTheme: (theme: Theme) => Promise<void>;
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

let pluginThemeVariables: string[] = [];

function clearPluginThemeVariables() {
  const root = document.documentElement;
  for (const variable of pluginThemeVariables) {
    root.style.removeProperty(variable);
  }
  pluginThemeVariables = [];
}

async function deactivatePluginTheme(): Promise<boolean> {
  try {
    await pluginApi.activateTheme(null);
    return true;
  } catch {
    console.warn('Failed to deactivate the active plugin theme');
    return false;
  }
}

export function applyPluginTheme(pluginTheme: PluginThemeDefinition | null) {
  const root = document.documentElement;
  clearPluginThemeVariables();
  if (!pluginTheme) {
    const theme = getInitialTheme();
    applyTheme(theme);
    useUiStore.setState({ theme, activePluginTheme: null });
    return;
  }
  root.classList.toggle('dark', pluginTheme.dark);
  for (const [key, value] of Object.entries(pluginTheme.colors)) {
    const variable = key.startsWith('--') ? key : `--${key.replaceAll('_', '-')}`;
    root.style.setProperty(variable, value);
    pluginThemeVariables.push(variable);
  }
  useUiStore.setState({
    theme: pluginTheme.dark ? 'dark' : 'light',
    activePluginTheme: pluginTheme.name,
  });
}

export const useUiStore = create<UiState>((set, get) => ({
  leftSidebarOpen: typeof window !== 'undefined' && window.innerWidth >= 768,
  settingsOpen: false,
  activeSettingsTab: 'overview',
  theme: getInitialTheme(),
  activePluginTheme: null,
  terminalOpen: false,

  toggleLeftSidebar: () => set((s) => ({ leftSidebarOpen: !s.leftSidebarOpen })),
  openSettings: () => set({ settingsOpen: true, activeSettingsTab: 'overview' }),
  closeSettings: () => set({ settingsOpen: false }),
  setActiveSettingsTab: (tab) => set({ activeSettingsTab: tab, settingsOpen: true }),
  toggleTheme: async () => {
    const next = get().theme === 'dark' ? 'light' : 'dark';
    if (!(await deactivatePluginTheme())) return;
    clearPluginThemeVariables();
    localStorage.setItem('echo-theme', next);
    applyTheme(next);
    set({ theme: next, activePluginTheme: null });
  },
  setTheme: async (theme) => {
    if (!(await deactivatePluginTheme())) return;
    clearPluginThemeVariables();
    localStorage.setItem('echo-theme', theme);
    applyTheme(theme);
    set({ theme, activePluginTheme: null });
  },
  openTerminal: () => set({ terminalOpen: true }),
  closeTerminal: () => set({ terminalOpen: false }),
  toggleTerminal: () => set((s) => ({ terminalOpen: !s.terminalOpen })),
}));

// Apply theme on load
if (typeof window !== 'undefined') {
  applyTheme(getInitialTheme());
}
