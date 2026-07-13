import { useEffect } from 'react';
import { Activity, Chrome, Globe2 } from 'lucide-react';
import { useBrowserEvents } from '../../hooks/useBrowserEvents';
import { useConversationStore } from '../../stores/conversationStore';
import { useBrowserStore } from '../../stores/browserStore';
import { BrowserStatus } from './BrowserStatus';
import { BrowserTabs } from './BrowserTabs';
import { BrowserToolbar } from './BrowserToolbar';
import { BrowserViewport } from './BrowserViewport';

export function BrowserPanel() {
  useBrowserEvents();
  const conversationId = useConversationStore((state) => state.activeId);
  const open = useBrowserStore((state) => state.open);
  const view = useBrowserStore((state) =>
    conversationId ? state.views[conversationId] : undefined
  );
  const store = useBrowserStore();
  useEffect(() => {
    void store.refreshChromeStatus();
  }, [store.refreshChromeStatus]);
  const activeTab =
    view?.session.tabs.find((tab) => tab.id === view.activeTabId) ?? view?.session.tabs[0];
  const busy =
    view?.session.status === 'navigating' ||
    view?.session.status === 'acting' ||
    view?.session.status === 'starting';

  if (!open) return null;

  const call = (fn: (id: string) => Promise<void>) => {
    if (conversationId) void fn(conversationId);
  };
  return (
    <>
      <div
        className="fixed inset-0 z-30 bg-black/25 lg:hidden"
        onClick={() => store.setOpen(false)}
      />
      <aside className="fixed inset-y-0 right-0 z-40 flex w-[min(92vw,680px)] min-w-0 flex-col border-l border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-xl lg:relative lg:z-20 lg:w-[clamp(360px,42vw,680px)] lg:shadow-none">
        <BrowserToolbar
          url={activeTab?.url ?? ''}
          busy={Boolean(busy)}
          onNavigate={(url) => conversationId && void store.navigate(conversationId, url)}
          onBack={() => call(store.back)}
          onReload={() => call(store.reload)}
          onStop={() => void store.stop()}
          onRefreshFrame={() => call(store.refreshFrame)}
          onClose={() => store.setOpen(false)}
          backend={view?.session.backend ?? 'managed'}
          chromeConnected={store.chromeConnected}
          onBackendChange={(backend) =>
            conversationId && void store.setBackend(conversationId, backend)
          }
        />
        <BrowserTabs
          tabs={view?.session.tabs ?? []}
          activeTabId={view?.activeTabId ?? null}
          onSelect={(index) => conversationId && void store.selectTab(conversationId, index)}
          onNew={() => call(store.newTab)}
          onClose={(index) => conversationId && void store.closeTab(conversationId, index)}
        />
        <BrowserViewport
          frame={view?.frame}
          busy={Boolean(busy)}
          interactive={Boolean(conversationId && view)}
          onClickAt={(x, y) => conversationId && void store.clickAt(conversationId, x, y)}
          onScroll={(deltaX, deltaY) =>
            conversationId && void store.scroll(conversationId, deltaX, deltaY)
          }
        />
        <footer className="flex h-7 shrink-0 items-center justify-between gap-3 border-t border-[var(--border-primary)] px-2.5">
          <BrowserStatus status={view?.session.status} error={view?.error} />
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
      </aside>
    </>
  );
}
