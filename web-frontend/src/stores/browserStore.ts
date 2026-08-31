import { create } from 'zustand';
import {
  browserExtensionDisposition,
  extensionApi,
  type BrowserExtensionDisposition,
} from '../api/endpoints';
import type { BrowserCommand, ExtensionRequestScope } from '../generated';
import { apiInvoke, errorMessage, isTauri } from '../lib/tauri-bridge';
import { viewAddress, viewAddressKey } from '../lib/viewAddress';

export type BrowserStatus =
  'starting' | 'ready' | 'navigating' | 'acting' | 'waiting_confirmation' | 'closed' | 'failed';

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

export type BrowserBackendResult =
  { status: 'settled'; message: null } | { status: 'pending' | 'failed'; message: string };

interface BrowserStore {
  views: Record<string, BrowserViewState>;
  commandErrors: Record<string, string>;
  commandPending: Record<string, string>;
  commandReceipts: Record<string, string>;
  chromeConnected: boolean;
  ingest: (event: BrowserEvent) => void;
  execute: (
    scope: ExtensionRequestScope,
    conversationId: string,
    command: BrowserCommand
  ) => Promise<BrowserCommandResult>;
  navigate: (scope: ExtensionRequestScope, conversationId: string, url: string) => Promise<void>;
  back: (scope: ExtensionRequestScope, conversationId: string) => Promise<void>;
  reload: (scope: ExtensionRequestScope, conversationId: string) => Promise<void>;
  refreshFrame: (scope: ExtensionRequestScope, conversationId: string) => Promise<void>;
  clickAt: (
    scope: ExtensionRequestScope,
    conversationId: string,
    x: number,
    y: number
  ) => Promise<void>;
  scroll: (
    scope: ExtensionRequestScope,
    conversationId: string,
    deltaX: number,
    deltaY: number
  ) => Promise<void>;
  stop: (scope: ExtensionRequestScope, conversationId: string) => Promise<void>;
  selectTab: (scope: ExtensionRequestScope, conversationId: string, index: number) => Promise<void>;
  newTab: (scope: ExtensionRequestScope, conversationId: string) => Promise<void>;
  closeTab: (scope: ExtensionRequestScope, conversationId: string, index: number) => Promise<void>;
  setBackend: (
    scope: ExtensionRequestScope,
    conversationId: string,
    backend: 'managed' | 'chrome'
  ) => Promise<BrowserBackendResult>;
  refreshChromeStatus: (scope: ExtensionRequestScope) => Promise<void>;
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

export type BrowserCommandResult =
  BrowserExtensionDisposition | { status: 'failed'; message: string };

let browserCommandSequence = 0;

function browserCommandId(prefix: string) {
  const randomId = globalThis.crypto?.randomUUID?.();
  if (randomId) return `${prefix}:${randomId}`;
  browserCommandSequence += 1;
  return `${prefix}:${Date.now()}:${browserCommandSequence}`;
}

async function invokeBrowser(
  scope: ExtensionRequestScope,
  conversationId: string,
  command: BrowserCommand
) {
  const workspaceId = scope.workspace_id;
  const addressKey = viewAddressKey(viewAddress(workspaceId, conversationId));
  useBrowserStore.setState((state) => ({
    commandErrors: { ...state.commandErrors, [addressKey]: '' },
    commandPending: { ...state.commandPending, [addressKey]: '' },
    commandReceipts: { ...state.commandReceipts, [addressKey]: '' },
  }));
  const actionId = browserCommandId(command.action);
  const receipt = await extensionApi.execute(scope, conversationId, {
    request_id: browserCommandId('browser-request'),
    operation_id: `browser-operation:${actionId}`,
    scope,
    extension: 'browser',
    command,
  });
  const disposition = browserExtensionDisposition(receipt);
  useBrowserStore.setState((state) => ({
    commandPending: {
      ...state.commandPending,
      [addressKey]: disposition.status === 'pending' ? disposition.message : '',
    },
    commandReceipts: {
      ...state.commandReceipts,
      [addressKey]: disposition.status === 'settled' ? disposition.message : '',
    },
  }));
  return disposition;
}

async function executeBrowser(
  set: (partial: Partial<BrowserStore> | ((state: BrowserStore) => Partial<BrowserStore>)) => void,
  scope: ExtensionRequestScope,
  conversationId: string,
  command: BrowserCommand
): Promise<BrowserCommandResult> {
  try {
    return await invokeBrowser(scope, conversationId, command);
  } catch (error) {
    setViewError(set, scope.workspace_id, conversationId, error);
    return { status: 'failed', message: errorMessage(error) };
  }
}

export const useBrowserStore = create<BrowserStore>((set) => ({
  views: {},
  commandErrors: {},
  commandPending: {},
  commandReceipts: {},
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
  execute: (scope, conversationId, command) => executeBrowser(set, scope, conversationId, command),
  navigate: async (scope, conversationId, url) => {
    await executeBrowser(set, scope, conversationId, { action: 'navigate', url });
  },
  back: async (scope, conversationId) => {
    await executeBrowser(set, scope, conversationId, { action: 'back' });
  },
  reload: async (scope, conversationId) => {
    await executeBrowser(set, scope, conversationId, { action: 'reload' });
  },
  refreshFrame: async (scope, conversationId) => {
    await executeBrowser(set, scope, conversationId, { action: 'screenshot' });
  },
  clickAt: async (scope, conversationId, x, y) => {
    await executeBrowser(set, scope, conversationId, { action: 'click', x, y });
  },
  scroll: async (scope, conversationId, deltaX, deltaY) => {
    await executeBrowser(set, scope, conversationId, {
      action: 'scroll',
      delta_x: deltaX,
      delta_y: deltaY,
    });
  },
  stop: async (scope, conversationId) => {
    await executeBrowser(set, scope, conversationId, { action: 'stop' });
  },
  selectTab: async (scope, conversationId, index) => {
    await executeBrowser(set, scope, conversationId, {
      action: 'tabs',
      tab_action: 'select',
      index,
      url: null,
    });
  },
  newTab: async (scope, conversationId) => {
    await executeBrowser(set, scope, conversationId, {
      action: 'tabs',
      tab_action: 'new',
      index: null,
      url: 'about:blank',
    });
  },
  closeTab: async (scope, conversationId, index) => {
    await executeBrowser(set, scope, conversationId, {
      action: 'tabs',
      tab_action: 'close',
      index,
      url: null,
    });
  },
  setBackend: async (scope, conversationId, backend) => {
    const workspaceId = scope.workspace_id;
    const addressKey = viewAddressKey(viewAddress(workspaceId, conversationId));
    const disposition = await executeBrowser(set, scope, conversationId, {
      action: backend,
    });
    set((state) => ({
      commandErrors: {
        ...state.commandErrors,
        [addressKey]: disposition.status === 'failed' ? disposition.message : '',
      },
      chromeConnected:
        disposition.status === 'settled' && backend === 'chrome' ? true : state.chromeConnected,
    }));
    return disposition.status === 'settled'
      ? { status: 'settled', message: null }
      : { status: disposition.status, message: disposition.message };
  },
  refreshChromeStatus: async (scope) => {
    if (!isTauri()) return;
    try {
      const status = await apiInvoke<{ connected: boolean }>('chrome_setup_status', {
        workspaceId: scope.workspace_id,
        workspaceGeneration: scope.workspace_generation,
      });
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
      commandPending: Object.fromEntries(
        Object.entries(state.commandPending).filter(([key]) => {
          try {
            const address = JSON.parse(key) as [string, string];
            return address[0] !== workspaceId;
          } catch {
            return true;
          }
        })
      ),
      commandReceipts: Object.fromEntries(
        Object.entries(state.commandReceipts).filter(([key]) => {
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
    commandPending: { ...state.commandPending, [addressKey]: '' },
    commandReceipts: { ...state.commandReceipts, [addressKey]: '' },
    views: updateSession(state.views, state.views[addressKey]?.session.id ?? '', (view) => ({
      ...view,
      error: errorMessage(error),
    })),
  }));
}
