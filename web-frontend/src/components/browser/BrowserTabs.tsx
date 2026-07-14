import { X } from 'lucide-react';
import type { BrowserTab } from '../../stores/browserStore';

export function BrowserTabs({
  tabs,
  activeTabId,
  onSelect,
  onClose,
}: {
  tabs: BrowserTab[];
  activeTabId: string | null;
  onSelect: (index: number) => void;
  onClose: (index: number) => void;
}) {
  if (tabs.length <= 1) return null;

  return (
    <div className="flex h-8 shrink-0 items-end border-b border-[var(--border-primary)] bg-[var(--bg-sidebar)] px-1">
      <div className="flex min-w-0 flex-1 overflow-x-auto">
        {tabs.map((tab) => (
          <div
            key={tab.id}
            role="tab"
            tabIndex={0}
            aria-selected={activeTabId === tab.id}
            onClick={() => onSelect(tab.index)}
            onKeyDown={(event) => {
              if (event.key === 'Enter') onSelect(tab.index);
            }}
            className={`group flex h-7 min-w-[92px] max-w-[180px] items-center gap-1.5 border-x border-t px-2 text-[11px] ${activeTabId === tab.id ? 'border-[var(--border-primary)] bg-[var(--bg-primary)] text-[var(--text-primary)]' : 'border-transparent text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)]'}`}
            title={tab.title ?? tab.url ?? '新标签页'}
          >
            <span className="min-w-0 flex-1 truncate text-left">
              {tab.title ?? tab.url ?? '新标签页'}
            </span>
            {tabs.length > 1 && (
              <button
                type="button"
                onClick={(event) => {
                  event.stopPropagation();
                  onClose(tab.index);
                }}
                onKeyDown={(event) => {
                  if (event.key === 'Enter') {
                    event.stopPropagation();
                    onClose(tab.index);
                  }
                }}
                className="flex h-4 w-4 items-center justify-center rounded-sm opacity-0 hover:bg-[var(--bg-hover)] group-hover:opacity-100"
                title="关闭标签页"
              >
                <X size={11} />
              </button>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}
