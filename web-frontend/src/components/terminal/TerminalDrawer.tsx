import { useEffect, useState } from 'react';
import { X, Plus, Minimize2 } from 'lucide-react';
import { useUiStore } from '../../stores/uiStore';
import { Terminal } from '../terminal/Terminal';
import { terminalApi } from '../../api/endpoints';
import { isTauri, apiInvoke } from '../../lib/tauri-bridge';
import { useToastStore } from '../../stores/toastStore';

interface TerminalTab {
  id: string;
  label: string;
}

export function TerminalDrawer() {
  const terminalOpen = useUiStore((s) => s.terminalOpen);
  const closeTerminal = useUiStore((s) => s.closeTerminal);

  const [tabs, setTabs] = useState<TerminalTab[]>([]);
  const [activeTabId, setActiveTabId] = useState<string | null>(null);
  const [height, setHeight] = useState(300);
  const [dragging, setDragging] = useState(false);
  const addToast = useToastStore((s) => s.addToast);

  // Create initial terminal session on first open
  useEffect(() => {
    if (terminalOpen && tabs.length === 0) {
      createNewTerminal();
    }
  }, [terminalOpen]);

  const createNewTerminal = async () => {
    const newId = `term-${Date.now()}`;
    const newLabel = `Terminal ${tabs.length + 1}`;

    if (isTauri()) {
      try {
        await apiInvoke('create_terminal', { id: newId, rows: 24, cols: 80 });
      } catch (e: unknown) {
        console.error('Failed to create terminal via IPC:', e);
        addToast('error', `创建终端失败: ${e instanceof Error ? e.message : 'Unknown error'}`);
        return;
      }
    } else {
      try {
        const session = await terminalApi.create();
        setTabs((prev) => [...prev, { id: session.id, label: newLabel }]);
        setActiveTabId(session.id);
        return;
      } catch (e: unknown) {
        console.error('Failed to create terminal session:', e);
        addToast('error', `创建终端失败: ${e instanceof Error ? e.message : 'Unknown error'}`);
        return;
      }
    }

    setTabs((prev) => [...prev, { id: newId, label: newLabel }]);
    setActiveTabId(newId);
  };

  const closeTab = async (tabId: string) => {
    if (isTauri()) {
      try {
        await apiInvoke('close_terminal', { id: tabId });
      } catch {
        // ignore
      }
    } else {
      try {
        await terminalApi.close(tabId);
      } catch {
        // ignore
      }
    }
    setTabs((prev) => {
      const next = prev.filter((t) => t.id !== tabId);
      if (activeTabId === tabId) {
        setActiveTabId(next.length > 0 ? next[next.length - 1].id : null);
      }
      if (next.length === 0) {
        closeTerminal();
      }
      return next;
    });
  };

  // Drag-to-resize
  useEffect(() => {
    if (!dragging) return;

    const handleMouseMove = (e: MouseEvent) => {
      const newHeight = window.innerHeight - e.clientY;
      setHeight(Math.max(150, Math.min(newHeight, window.innerHeight - 100)));
    };

    const handleMouseUp = () => {
      setDragging(false);
    };

    document.addEventListener('mousemove', handleMouseMove);
    document.addEventListener('mouseup', handleMouseUp);
    return () => {
      document.removeEventListener('mousemove', handleMouseMove);
      document.removeEventListener('mouseup', handleMouseUp);
    };
  }, [dragging]);

  if (!terminalOpen) return null;

  const activeTab = tabs.find((t) => t.id === activeTabId);

  return (
    <div
      className="flex flex-col border-t border-[var(--border-primary)] bg-[var(--bg-primary)]"
      style={{ height: `${height}px` }}
    >
      {/* Resize handle */}
      <div
        className="h-1 cursor-row-resize hover:bg-[var(--accent)]/30 transition-colors"
        onMouseDown={() => setDragging(true)}
      />

      {/* Tab bar */}
      <div className="flex items-center justify-between border-b border-[var(--border-primary)] bg-[var(--bg-sidebar)] px-2">
        <div className="flex items-center gap-0.5 overflow-x-auto">
          {tabs.map((tab) => (
            <div
              key={tab.id}
              className={`group flex items-center gap-1.5 rounded-t-md px-3 py-1.5 text-xs cursor-pointer transition-colors
                ${
                  tab.id === activeTabId
                    ? 'bg-[var(--bg-primary)] text-[var(--text-primary)] border-b-2 border-b-[var(--accent)]'
                    : 'text-[var(--text-secondary)] hover:bg-[var(--bg-sidebar-hover)] hover:text-[var(--text-primary)]'
                }`}
              onClick={() => setActiveTabId(tab.id)}
            >
              <span>{tab.label}</span>
              <button
                onClick={(e) => {
                  e.stopPropagation();
                  closeTab(tab.id);
                }}
                className="ml-1 rounded-md p-0.5 opacity-0 group-hover:opacity-100 hover:bg-[var(--bg-hover)] transition-opacity"
              >
                <X size={10} />
              </button>
            </div>
          ))}
          <button
            onClick={createNewTerminal}
            className="rounded-md p-1 text-[var(--text-tertiary)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-sidebar-hover)] transition-colors"
            title="New terminal"
          >
            <Plus size={12} />
          </button>
        </div>
        <div className="flex items-center gap-1">
          <button
            onClick={() => setHeight((h) => Math.max(150, h - 50))}
            className="rounded-md p-1 text-[var(--text-tertiary)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-sidebar-hover)] transition-colors"
            title="Shrink"
          >
            <Minimize2 size={12} />
          </button>
          <button
            onClick={closeTerminal}
            className="rounded-md p-1 text-[var(--text-tertiary)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-sidebar-hover)] transition-colors"
            title="Close terminal"
          >
            <X size={14} />
          </button>
        </div>
      </div>

      {/* Terminal content — min-h-0 is critical for xterm.js flex sizing */}
      <div className="flex-1 min-h-0 overflow-hidden">
        {activeTab ? (
          <Terminal sessionId={activeTab.id} />
        ) : (
          <div className="flex h-full items-center justify-center text-xs text-[var(--text-tertiary)]">
            No terminal sessions. Click + to create one.
          </div>
        )}
      </div>
    </div>
  );
}
