import { useCallback, useEffect, useState } from 'react';
import { Activity, Chrome, Globe2 } from 'lucide-react';
import { useBrowserEvents } from '../../hooks/useBrowserEvents';
import { useConversationStore } from '../../stores/conversationStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useBrowserStore } from '../../stores/browserStore';
import { BrowserStatus } from './BrowserStatus';
import { BrowserTabs } from './BrowserTabs';
import { BrowserToolbar } from './BrowserToolbar';
import { BrowserViewport } from './BrowserViewport';
import { ChromeSetupDialog } from './ChromeSetupDialog';

export function BrowserPanel() {
  useBrowserEvents();
  const [chromeSetupOpen, setChromeSetupOpen] = useState(false);
  const conversationId = useConversationStore((state) => state.activeId);
  const workspaceId = useWorkspaceStore((state) => state.current?.id);
  const browserScopeId = conversationId ?? `ui-preview:${workspaceId ?? 'global'}`;
  const view = useBrowserStore((state) => state.views[browserScopeId]);
  const store = useBrowserStore();
  const handleChromeConnectionChange = useCallback((connected: boolean) => {
    useBrowserStore.setState({ chromeConnected: connected });
  }, []);
  useEffect(() => {
    void store.refreshChromeStatus();
  }, [store.refreshChromeStatus]);
  useEffect(() => {
    if (!view || view.session.status !== 'ready') return;
    const timer = window.setInterval(() => void store.refreshFrame(browserScopeId), 1500);
    return () => window.clearInterval(timer);
  }, [browserScopeId, store.refreshFrame, view?.session.status]);
  const activeTab =
    view?.session.tabs.find((tab) => tab.id === view.activeTabId) ?? view?.session.tabs[0];
  const busy =
    view?.session.status === 'navigating' ||
    view?.session.status === 'acting' ||
    view?.session.status === 'starting';

  const call = (fn: (id: string) => Promise<void>) => {
    void fn(browserScopeId);
  };
  return (
    <>
      <div className="flex h-full min-h-0 flex-col">
        <BrowserToolbar
          url={activeTab?.url ?? ''}
          busy={Boolean(busy)}
          onNavigate={(url) => void store.navigate(browserScopeId, url)}
          onBack={() => call(store.back)}
          onReload={() => call(store.reload)}
          onStop={() => void store.stop()}
          onRefreshFrame={() => call(store.refreshFrame)}
          onNewTab={() => call(store.newTab)}
          backend={view?.session.backend ?? 'managed'}
          chromeConnected={store.chromeConnected}
          onBackendChange={(backend) => {
            void store.setBackend(browserScopeId, backend).then((error) => {
              if (error && backend === 'chrome') setChromeSetupOpen(true);
            });
          }}
          onChromeSetup={() => setChromeSetupOpen(true)}
        />
        <BrowserTabs
          tabs={view?.session.tabs ?? []}
          activeTabId={view?.activeTabId ?? null}
          onSelect={(index) => void store.selectTab(browserScopeId, index)}
          onClose={(index) => void store.closeTab(browserScopeId, index)}
        />
        <BrowserViewport
          frame={view?.frame}
          busy={Boolean(busy)}
          clickable={Boolean(view && view.session.backend !== 'chrome')}
          scrollable={Boolean(view)}
          onClickAt={(x, y) => void store.clickAt(browserScopeId, x, y)}
          onScroll={(deltaX, deltaY) => void store.scroll(browserScopeId, deltaX, deltaY)}
        />
        {(busy ||
          view?.session.status === 'waiting_confirmation' ||
          view?.error ||
          store.commandErrors[browserScopeId] ||
          Boolean(view?.diagnostics.length) ||
          view?.session.backend === 'chrome') && (
          <footer className="flex h-7 shrink-0 items-center justify-between gap-3 border-t border-[var(--border-primary)] px-2.5">
            <BrowserStatus
              status={view?.session.status}
              error={view?.error ?? store.commandErrors[browserScopeId]}
            />
            <div className="flex min-w-0 items-center gap-2 text-[10px] text-[var(--text-tertiary)]">
              {Boolean(view?.diagnostics.length) && (
                <span
                  className="flex shrink-0 items-center gap-1"
                  title={`诊断记录 ${view?.diagnostics.length ?? 0}`}
                >
                  <Activity size={11} />
                  {view?.diagnostics.length}
                </span>
              )}
              {view?.session.backend === 'chrome' && (
                <span className="flex shrink-0 items-center gap-1" title="使用已授权 Chrome 标签页">
                  <Chrome size={11} />
                  Chrome
                </span>
              )}
              <span className="flex min-w-0 items-center gap-1">
                <Globe2 size={11} />
                <span className="truncate">{activeTab?.url ?? 'about:blank'}</span>
              </span>
            </div>
          </footer>
        )}
      </div>
      {chromeSetupOpen && (
        <ChromeSetupDialog
          onClose={() => setChromeSetupOpen(false)}
          onConnectionChange={handleChromeConnectionChange}
          onUseChrome={async () => {
            const error = await store.setBackend(browserScopeId, 'chrome');
            if (!error) setChromeSetupOpen(false);
            return error;
          }}
        />
      )}
    </>
  );
}
