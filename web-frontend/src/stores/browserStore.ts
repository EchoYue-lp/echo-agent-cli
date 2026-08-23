import { create } from 'zustand';
import { apiInvoke, errorMessage, isTauri } from '../lib/tauri-bridge';
import { viewAddress, viewAddressKey } from '../lib/viewAddress';

export type BrowserStatus =
  | 'starting'
  | 'ready'
  | 'navigating'
  | 'acting'
  | 'waiting_confirmation'
  | 'closed'
  | 'failed';

export interface BrowserTab {
  id: string;
  index: number;
  owner_run_id?: string | null;
  url?: string | null;
  title?: string | null;
}

export interface BrowserSession {
  id: string;
  workspace_id: string;
  conversation_id: string;
  status: BrowserStatus;
  backend?: 'managed' | 'chrome';
  developer_mode?: boolean;
  tabs: BrowserTab[];
}

interface BrowserFrame {
  data_url: string;
  mime_type: string;
}

export type BrowserEvent =
  | { type: 'session_started'; session: BrowserSession }
  | { type: 'session_updated'; session: BrowserSession }
  | { type: 'tab_opened'; session_id: string; tab: BrowserTab }
  | { type: 'navigation_started'; session_id: string; tab_id: string; url: string }
  | { type: 'navigation_completed'; session_id: string; tab_id: string; url: string }
  | { type: 'snapshot'; observation: { session_id: string; tab_id: string } }
  | {
      type: 'screenshot';
      observation: { session_id: string; tab_id: string; captured_at: string };
      frame?: BrowserFrame | null;
    }
  | {
      type: 'diagnostic';
      category: string;
      observation: { session_id: string; tab_id: string; summary: string; captured_at: string };
    }
  | { type: 'backend_changed'; session_id: string; backend: 'managed' | 'chrome' }
  | {
      type: 'confirmation_requested';
      session_id: string;
      tab_id: string;
      risk: string;
      summary: string;
    }
  | { type: 'confirmation_resolved'; session_id: string; tab_id: string; approved: boolean }
  | { type: 'action_started'; session_id: string; tab_id: string; action: string }
  | { type: 'action_completed'; session_id: string; tab_id: string; action: string }
  | { type: 'action_failed'; session_id: string; tab_id: string; action: string; error: string }
  | { type: 'session_closed'; session_id: string };

interface BrowserViewState {
  session: BrowserSession;
  activeTabId: string | null;
  frame: string | null;
  frameCapturedAt: string | null;
  error: string | null;
  diagnostics: Array<{ category: string; summary: string; capturedAt: string }>;
}

interface BrowserStore {
  views: Record<string, BrowserViewState>;
  commandErrors: Record<string, string>;
  chromeConnected: boolean;
  ingest: (event: BrowserEvent) => void;
  navigate: (workspaceId: string, conversationId: string, url: string) => Promise<void>;
  back: (workspaceId: string, conversationId: string) => Promise<void>;
  reload: (workspaceId: string, conversationId: string) => Promise<void>;
  refreshFrame: (workspaceId: string, conversationId: string) => Promise<void>;
  clickAt: (workspaceId: string, conversationId: string, x: number, y: number) => Promise<void>;
  scroll: (
    workspaceId: string,
    conversationId: string,
    deltaX: number,
    deltaY: number
  ) => Promise<void>;
  stop: () => Promise<void>;
  selectTab: (workspaceId: string, conversationId: string, index: number) => Promise<void>;
  newTab: (workspaceId: string, conversationId: string) => Promise<void>;
  closeTab: (workspaceId: string, conversationId: string, index: number) => Promise<void>;
  setBackend: (
    workspaceId: string,
    conversationId: string,
    backend: 'managed' | 'chrome'
  ) => Promise<string | null>;
  refreshChromeStatus: () => Promise<void>;
  clearWorkspace: (workspaceId: string) => void;
}

function updateSession(
  views: Record<string, BrowserViewState>,
  sessionId: string,
  updater: (view: BrowserViewState) => BrowserViewState
) {
  const entry = Object.entries(views).find(([, view]) => view.session.id === sessionId);
  if (!entry) return views;
  const [addressKey, view] = entry;
  return { ...views, [addressKey]: updater(view) };
}

async function invokeBrowser(command: string, args?: Record<string, unknown>) {
  if (!isTauri()) return;
  await apiInvoke<void>(command, args);
}

export const useBrowserStore = create<BrowserStore>((set) => ({
  views: {},
  commandErrors: {},
  chromeConnected: false,
  ingest: (event) =>
    set((state) => {
      if (event.type === 'session_started' || event.type === 'session_updated') {
        const firstTab = event.session.tabs[0];
        return {
          views: {
            ...state.views,
            [viewAddressKey(
              viewAddress(event.session.workspace_id, event.session.conversation_id)
            )]: {
              session: event.session,
              activeTabId: firstTab?.id ?? null,
              frame: null,
              frameCapturedAt: null,
              error: null,
              diagnostics: [],
            },
          },
        };
      }
      const sessionId =
        event.type === 'snapshot' || event.type === 'screenshot' || event.type === 'diagnostic'
          ? event.observation.session_id
          : event.session_id;
      const views = updateSession(state.views, sessionId, (view) => {
        if (event.type === 'tab_opened') {
          return {
            ...view,
            session: { ...view.session, tabs: [...view.session.tabs, event.tab] },
            activeTabId: event.tab.id,
          };
        }
        if (event.type === 'backend_changed') {
          return {
            ...view,
            frame: null,
            session: { ...view.session, backend: event.backend, status: 'ready' },
          };
        }
        if (event.type === 'navigation_started') {
          return {
            ...view,
            activeTabId: event.tab_id,
            error: null,
            session: { ...view.session, status: 'navigating' },
          };
        }
        if (event.type === 'navigation_completed') {
          return {
            ...view,
            activeTabId: event.tab_id,
            session: {
              ...view.session,
              status: 'ready',
              tabs: view.session.tabs.map((tab) =>
                tab.id === event.tab_id ? { ...tab, url: event.url } : tab
              ),
            },
          };
        }
        if (event.type === 'action_started') {
          return {
            ...view,
            activeTabId: event.tab_id,
            error: null,
            session: { ...view.session, status: 'acting' },
          };
        }
        if (event.type === 'confirmation_requested') {
          return {
            ...view,
            activeTabId: event.tab_id,
            error: null,
            session: { ...view.session, status: 'waiting_confirmation' },
          };
        }
        if (event.type === 'confirmation_resolved') {
          return {
            ...view,
            session: { ...view.session, status: event.approved ? 'acting' : 'ready' },
          };
        }
        if (event.type === 'action_completed') {
          return { ...view, session: { ...view.session, status: 'ready' } };
        }
        if (event.type === 'action_failed') {
          return {
            ...view,
            error: event.error,
            session: { ...view.session, status: 'failed' },
          };
        }
        if (event.type === 'screenshot' && event.frame?.data_url) {
          return {
            ...view,
            activeTabId: event.observation.tab_id,
            frame: event.frame.data_url,
            frameCapturedAt: event.observation.captured_at,
          };
        }
        if (event.type === 'diagnostic') {
          return {
            ...view,
            diagnostics: [
              ...view.diagnostics.slice(-19),
              {
                category: event.category,
                summary: event.observation.summary,
                capturedAt: event.observation.captured_at,
              },
            ],
          };
        }
        if (event.type === 'session_closed') {
          return { ...view, session: { ...view.session, status: 'closed' } };
        }
        return view;
      });
      return { views };
    }),
  navigate: async (workspaceId, conversationId, url) => {
    const addressKey = viewAddressKey(viewAddress(workspaceId, conversationId));
    set((state) => ({
      commandErrors: { ...state.commandErrors, [addressKey]: '' },
    }));
    try {
      await invokeBrowser('browser_navigate', { workspaceId, conversationId, url });
    } catch (error) {
      set((state) => ({
        views: updateSession(state.views, state.views[addressKey]?.session.id ?? '', (view) => ({
          ...view,
          error: errorMessage(error),
        })),
      }));
    }
  },
  back: async (workspaceId, conversationId) => {
    try {
      await invokeBrowser('browser_back', { workspaceId, conversationId });
    } catch (error) {
      setViewError(set, workspaceId, conversationId, error);
    }
  },
  reload: async (workspaceId, conversationId) => {
    try {
      await invokeBrowser('browser_reload', { workspaceId, conversationId });
    } catch (error) {
      setViewError(set, workspaceId, conversationId, error);
    }
  },
  refreshFrame: async (workspaceId, conversationId) => {
    try {
      await invokeBrowser('browser_screenshot', { workspaceId, conversationId });
    } catch (error) {
      setViewError(set, workspaceId, conversationId, error);
    }
  },
  clickAt: async (workspaceId, conversationId, x, y) => {
    try {
      await invokeBrowser('browser_click_at', { workspaceId, conversationId, x, y });
    } catch (error) {
      setViewError(set, workspaceId, conversationId, error);
    }
  },
  scroll: async (workspaceId, conversationId, deltaX, deltaY) => {
    try {
      await invokeBrowser('browser_scroll', { workspaceId, conversationId, deltaX, deltaY });
    } catch (error) {
      setViewError(set, workspaceId, conversationId, error);
    }
  },
  stop: () => invokeBrowser('browser_stop'),
  selectTab: async (workspaceId, conversationId, index) => {
    try {
      await invokeBrowser('browser_tabs', {
        workspaceId,
        conversationId,
        action: 'select',
        index,
      });
    } catch (error) {
      setViewError(set, workspaceId, conversationId, error);
    }
  },
  newTab: async (workspaceId, conversationId) => {
    try {
      await invokeBrowser('browser_tabs', {
        workspaceId,
        conversationId,
        action: 'new',
        url: 'about:blank',
      });
    } catch (error) {
      setViewError(set, workspaceId, conversationId, error);
    }
  },
  closeTab: async (workspaceId, conversationId, index) => {
    try {
      await invokeBrowser('browser_tabs', { workspaceId, conversationId, action: 'close', index });
    } catch (error) {
      setViewError(set, workspaceId, conversationId, error);
    }
  },
  setBackend: async (workspaceId, conversationId, backend) => {
    const addressKey = viewAddressKey(viewAddress(workspaceId, conversationId));
    try {
      await invokeBrowser('browser_set_backend', { workspaceId, conversationId, backend });
      set((state) => ({
        commandErrors: { ...state.commandErrors, [addressKey]: '' },
        chromeConnected: backend === 'chrome' ? true : state.chromeConnected,
      }));
      return null;
    } catch (error) {
      setViewError(set, workspaceId, conversationId, error);
      return errorMessage(error);
    }
  },
  refreshChromeStatus: async () => {
    if (!isTauri()) return;
    try {
      const status = await apiInvoke<{ connected: boolean }>('chrome_setup_status');
      set({ chromeConnected: status.connected });
    } catch {
      set({ chromeConnected: false });
    }
  },
  clearWorkspace: (workspaceId) =>
    set((state) => ({
      views: Object.fromEntries(
        Object.entries(state.views).filter(([, view]) => view.session.workspace_id !== workspaceId)
      ),
      commandErrors: Object.fromEntries(
        Object.entries(state.commandErrors).filter(([key]) => {
          try {
            const address = JSON.parse(key) as [string, string];
            return address[0] !== workspaceId;
          } catch {
            return true;
          }
        })
      ),
    })),
}));

function setViewError(
  set: (partial: Partial<BrowserStore> | ((state: BrowserStore) => Partial<BrowserStore>)) => void,
  workspaceId: string,
  conversationId: string,
  error: unknown
) {
  const addressKey = viewAddressKey(viewAddress(workspaceId, conversationId));
  set((state) => ({
    commandErrors: {
      ...state.commandErrors,
      [addressKey]: errorMessage(error),
    },
    views: updateSession(state.views, state.views[addressKey]?.session.id ?? '', (view) => ({
      ...view,
      error: errorMessage(error),
    })),
  }));
}
