import { FileJson, Workflow } from 'lucide-react';
import type { ReactNode } from 'react';
import { useRightWorkspaceStore } from '../../stores/rightWorkspaceStore';
import { ExtractPanel } from '../extract/ExtractPanel';
import { WorkflowPanel } from '../workflow/WorkflowPanel';

export function AutomationPanel() {
  const view = useRightWorkspaceStore((state) => state.automationView);
  const setAutomationView = useRightWorkspaceStore((state) => state.setAutomationView);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div
        className="flex h-10 shrink-0 items-center border-b border-[var(--border-secondary)] px-2"
        role="tablist"
        aria-label="自动化工具"
      >
        <AutomationTab
          active={view === 'workflows'}
          icon={<Workflow size={13} />}
          label="工作流"
          onClick={() => setAutomationView('workflows')}
        />
        <AutomationTab
          active={view === 'extract'}
          icon={<FileJson size={13} />}
          label="结构化提取"
          onClick={() => setAutomationView('extract')}
        />
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {view === 'workflows' ? <WorkflowPanel /> : <ExtractPanel />}
      </div>
    </div>
  );
}

function AutomationTab({
  active,
  icon,
  label,
  onClick,
}: {
  active: boolean;
  icon: ReactNode;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      role="tab"
      aria-selected={active}
      onClick={onClick}
      className={`relative flex h-10 items-center gap-1.5 px-3 text-xs transition-colors ${
        active
          ? 'text-[var(--text-primary)] after:absolute after:inset-x-2 after:bottom-0 after:h-0.5 after:bg-[var(--accent)]'
          : 'text-[var(--text-tertiary)] hover:text-[var(--text-primary)]'
      }`}
    >
      {icon}
      {label}
    </button>
  );
}
