import { type ReactNode } from 'react';
import { PanelLeftClose, PanelLeftOpen } from 'lucide-react';
import { useUiStore } from '../../stores/uiStore';

export function AppLayout({ left, center }: { left: ReactNode; center: ReactNode }) {
  const { leftSidebarOpen, toggleLeftSidebar } = useUiStore();

  const closeSidebarMobile = () => {
    if (window.innerWidth < 768 && leftSidebarOpen) {
      toggleLeftSidebar();
    }
  };

  return (
    <div className="flex h-screen w-screen bg-[var(--bg-chat)]">
      {/* Left Sidebar - Mobile overlay with blur */}
      {leftSidebarOpen && (
        <div
          className="fixed inset-0 z-40 md:hidden"
          style={{ background: 'var(--bg-overlay)' }}
          onClick={toggleLeftSidebar}
        />
      )}
      <div
        className={`shrink-0 overflow-hidden transition-all duration-300 ease-in-out
          max-md:fixed max-md:inset-y-0 max-md:left-0 max-md:z-50
          ${leftSidebarOpen ? 'border-r border-[var(--border-primary)] bg-[var(--bg-sidebar)]' : ''}`}
        style={{ width: leftSidebarOpen ? '280px' : '0px' }}
      >
        <div className="h-full w-[280px]">{left}</div>
      </div>

      {/* Center */}
      <div className="relative flex min-w-0 flex-1 flex-col min-h-0">
        {/* Left toggle */}
        <button
          onClick={toggleLeftSidebar}
          className="absolute left-3 top-3 z-20 flex h-7 w-7 items-center justify-center rounded-lg text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          title={leftSidebarOpen ? '关闭侧边栏' : '打开侧边栏'}
        >
          {leftSidebarOpen ? <PanelLeftClose size={14} /> : <PanelLeftOpen size={14} />}
        </button>
        <div onClick={closeSidebarMobile} className="flex flex-1 flex-col min-h-0">
          {center}
        </div>
      </div>

    </div>
  );
}
