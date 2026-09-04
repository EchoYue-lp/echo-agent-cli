import { useEffect, type ReactNode } from 'react';
import { PanelLeftClose, PanelLeftOpen } from 'lucide-react';
import { useUiStore } from '../../stores/uiStore';
import { TerminalDrawer } from '../terminal/TerminalDrawer';

export function AppLayout({
  left,
  center,
  right,
}: {
  left: ReactNode;
  center: ReactNode;
  right?: ReactNode;
}) {
  const { leftSidebarOpen, toggleLeftSidebar } = useUiStore();

  useEffect(() => {
    if (!leftSidebarOpen) return;
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && window.innerWidth < 768) toggleLeftSidebar();
    };
    window.addEventListener('keydown', handleEscape);
    return () => window.removeEventListener('keydown', handleEscape);
  }, [leftSidebarOpen, toggleLeftSidebar]);

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-[var(--bg-chat)]">
      {/* Left Sidebar - Mobile overlay with blur */}
      {leftSidebarOpen && (
        <button
          type="button"
          aria-label="关闭侧边栏"
          className="fixed inset-0 z-40 md:hidden"
          style={{ background: 'var(--bg-overlay)' }}
          onClick={toggleLeftSidebar}
        />
      )}
      <div
        className={`shrink-0 overflow-hidden transition-all duration-300 ease-in-out
          max-md:fixed max-md:inset-y-0 max-md:left-0 max-md:z-50
          ${leftSidebarOpen ? 'border-r border-[var(--border-primary)] bg-[var(--bg-sidebar)]' : ''}`}
        style={{ width: leftSidebarOpen ? '264px' : '0px' }}
      >
        <div className="h-full w-[264px]">{left}</div>
      </div>

      {/* Center + Terminal drawer */}
      <div className="relative flex min-w-0 flex-1 flex-col min-h-0">
        {/* Left toggle */}
        <button
          onClick={toggleLeftSidebar}
          aria-label={leftSidebarOpen ? '关闭侧边栏' : '打开侧边栏'}
          className="absolute left-3 top-3 z-20 flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          title={leftSidebarOpen ? '关闭侧边栏' : '打开侧边栏'}
        >
          {leftSidebarOpen ? <PanelLeftClose size={14} /> : <PanelLeftOpen size={14} />}
        </button>
        <div className="flex min-h-0 flex-1">
          <div className="flex min-w-[320px] flex-1 flex-col">{center}</div>
          {right}
        </div>
        {/* Terminal drawer at bottom */}
        <TerminalDrawer />
      </div>
    </div>
  );
}
