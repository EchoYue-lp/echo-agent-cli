import { create } from 'zustand';
import { apiInvoke, errorMessage, isTauri } from '../lib/tauri-bridge';

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
  open: boolean;
  views: Record<string, BrowserViewState>;
  chromeConnected: boolean;
  toggle: () => void;
  setOpen: (open: boolean) => void;
  ingest: (event: BrowserEvent) => void;
  navigate: (conversationId: string, url: string) => Promise<void>;
  back: (conversationId: string) => Promise<void>;
  reload: (conversationId: string) => Promise<void>;
  refreshFrame: (conversationId: string) => Promise<void>;
  stop: () => Promise<void>;
  selectTab: (conversationId: string, index: number) => Promise<void>;
  newTab: (conversationId: string) => Promise<void>;
  closeTab: (conversationId: string, index: number) => Promise<void>;
  setBackend: (conversationId: string, backend: 'managed' | 'chrome') => Promise<void>;
  refreshChromeStatus: () => Promise<void>;
}

function updateSession(
  views: Record<string, BrowserViewState>,
  sessionId: string,
  updater: (view: BrowserViewState) => BrowserViewState
) {
  const entry = Object.entries(views).find(([, view]) => view.session.id === sessionId);
  if (!entry) return views;
  const [conversationId, view] = entry;
  return { ...views, [conversationId]: updater(view) };
}

async function invokeBrowser(command: string, args?: Record<string, unknown>) {
  if (!isTauri()) return;
  await apiInvoke<void>(command, args);
}

export const useBrowserStore = create<BrowserStore>((set) => ({
  open: false,
  views: {},
  chromeConnected: false,
  toggle: () => set((state) => ({ open: !state.open })),
  setOpen: (open) => set({ open }),
  ingest: (event) =>
    set((state) => {
      if (event.type === 'session_started') {
        const firstTab = event.session.tabs[0];
        return {
          open: true,
          views: {
            ...state.views,
            [event.session.conversation_id]: {
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
  navigate: async (conversationId, url) => {
    try {
      await invokeBrowser('browser_navigate', { conversationId, url });
    } catch (error) {
      set((state) => ({
        views: updateSession(
          state.views,
          state.views[conversationId]?.session.id ?? '',
          (view) => ({
            ...view,
            error: errorMessage(error),
          })
        ),
      }));
    }
  },
  back: (conversationId) => invokeBrowser('browser_back', { conversationId }),
  reload: (conversationId) => invokeBrowser('browser_reload', { conversationId }),
  refreshFrame: (conversationId) => invokeBrowser('browser_screenshot', { conversationId }),
  stop: () => invokeBrowser('browser_stop'),
  selectTab: (conversationId, index) =>
    invokeBrowser('browser_tabs', { conversationId, action: 'select', index }),
  newTab: (conversationId) =>
    invokeBrowser('browser_tabs', { conversationId, action: 'new', url: 'about:blank' }),
  closeTab: (conversationId, index) =>
    invokeBrowser('browser_tabs', { conversationId, action: 'close', index }),
  setBackend: async (conversationId, backend) => {
    try {
      await invokeBrowser('browser_set_backend', { conversationId, backend });
    } catch (error) {
      set((state) => ({
        views: updateSession(
          state.views,
          state.views[conversationId]?.session.id ?? '',
          (view) => ({ ...view, error: errorMessage(error) })
        ),
      }));
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
}));
