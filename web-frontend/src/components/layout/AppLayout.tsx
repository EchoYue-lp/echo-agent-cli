import { type ReactNode } from 'react';
import { PanelLeftClose, PanelLeftOpen, PanelRightClose, PanelRightOpen } from 'lucide-react';
import { useUiStore } from '../../stores/uiStore';

export function AppLayout({ left, center, right }: { left: ReactNode; center: ReactNode; right: ReactNode }) {
  const { leftSidebarOpen, rightPanelOpen, toggleLeftSidebar, toggleRightPanel } = useUiStore();

  const closeSidebarMobile = () => {
    if (window.innerWidth < 768 && leftSidebarOpen) {
      toggleLeftSidebar();
    }
  };

  return (
    <div className="flex h-screen w-screen overflow-hidden" style={{ background: 'var(--bg-chat)' }}>
      {/* Left Sidebar - Desktop: inline, Mobile: overlay */}
      {leftSidebarOpen && (
        <div
          className="fixed inset-0 z-40 md:hidden"
          style={{ background: 'var(--bg-overlay)' }}
          onClick={toggleLeftSidebar}
        />
      )}
      <div
        className="shrink-0 overflow-hidden transition-all duration-300 ease-in-out max-md:fixed max-md:inset-y-0 max-md:left-0 max-md:z-50"
        style={{
          width: leftSidebarOpen ? '280px' : '0px',
          borderRight: leftSidebarOpen ? '1px solid var(--border-primary)' : 'none',
          background: 'var(--bg-sidebar)',
        }}
      >
        <div className="h-full w-[280px]">{left}</div>
      </div>

      {/* Center - Chat */}
      <div className="relative flex min-w-0 flex-1 flex-col">
        {/* Left sidebar toggle */}
        <button
          onClick={toggleLeftSidebar}
          className="absolute left-3 top-3 z-20 rounded-lg p-2 transition-all"
          style={{
            background: 'var(--bg-primary)',
            color: 'var(--text-secondary)',
            boxShadow: 'var(--shadow-sm)',
            border: '1px solid var(--border-primary)',
          }}
        >
          {leftSidebarOpen ? <PanelLeftClose size={15} /> : <PanelLeftOpen size={15} />}
        </button>
        <div onClick={closeSidebarMobile} className="flex flex-1 flex-col">
          {center}
        </div>
      </div>

      {/* Right Panel - Desktop: inline, Mobile: overlay */}
      {rightPanelOpen && (
        <div
          className="fixed inset-0 z-40 md:hidden"
          style={{ background: 'var(--bg-overlay)' }}
          onClick={toggleRightPanel}
        />
      )}
      <div
        className="shrink-0 overflow-hidden transition-all duration-300 ease-in-out max-md:fixed max-md:inset-y-0 max-md:right-0 max-md:z-50"
        style={{
          width: rightPanelOpen ? '340px' : '0px',
          borderLeft: rightPanelOpen ? '1px solid var(--border-primary)' : 'none',
          background: 'var(--bg-primary)',
        }}
      >
        <div className="flex h-full w-[340px] max-md:w-[100vw] flex-col">
          <div
            className="flex items-center justify-between px-3 py-2.5"
            style={{ borderBottom: '1px solid var(--border-primary)' }}
          >
            <span className="text-xs font-semibold uppercase tracking-wider" style={{ color: 'var(--text-tertiary)' }}>
              Details
            </span>
            <button
              onClick={toggleRightPanel}
              className="rounded-md p-1 transition-colors"
              style={{ color: 'var(--text-tertiary)' }}
            >
              <PanelRightClose size={15} />
            </button>
          </div>
          {right}
        </div>
      </div>

      {/* Right panel toggle (when closed) */}
      {!rightPanelOpen && (
        <button
          onClick={toggleRightPanel}
          className="absolute right-3 top-3 z-20 rounded-lg p-2 transition-all"
          style={{
            background: 'var(--bg-primary)',
            color: 'var(--text-secondary)',
            boxShadow: 'var(--shadow-sm)',
            border: '1px solid var(--border-primary)',
          }}
        >
          <PanelRightOpen size={15} />
        </button>
      )}
    </div>
  );
}
