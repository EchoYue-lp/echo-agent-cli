import { ArrowLeft, BookOpen, FileJson, FlaskConical, Workflow } from 'lucide-react';
import { useWorkspaceViewStore } from '../../stores/workspaceViewStore';
import { ChatPanel } from '../chat/ChatPanel';
import AnalysisPanel from '../analysis/AnalysisPanel';
import { PaperPanel } from '../papers/PaperPanel';
import { WorkflowPanel } from '../workflow/WorkflowPanel';
import { ExtractPanel } from '../extract/ExtractPanel';

export function PrimaryWorkspace() {
  const activeView = useWorkspaceViewStore((state) => state.activeView);
  const openChat = useWorkspaceViewStore((state) => state.openChat);

  const workbench =
    activeView === 'analysis'
      ? { label: '分析', icon: <FlaskConical size={14} />, content: <AnalysisPanel /> }
      : activeView === 'research'
        ? { label: '研究', icon: <BookOpen size={14} />, content: <PaperPanel /> }
        : activeView === 'workflow'
          ? { label: '工作流', icon: <Workflow size={14} />, content: <WorkflowPanel /> }
          : activeView === 'extract'
            ? { label: '结构化提取', icon: <FileJson size={14} />, content: <ExtractPanel /> }
            : null;

  return (
    <div className="relative h-full min-h-0 min-w-0">
      <div
        className={activeView === 'chat' ? 'h-full' : 'hidden'}
        aria-hidden={activeView !== 'chat'}
      >
        <ChatPanel />
      </div>
      {workbench && (
        <section className="absolute inset-0 flex min-h-0 min-w-0 flex-col bg-[var(--bg-primary)]">
          <header className="flex h-11 shrink-0 items-center gap-2 border-b border-[var(--border-secondary)] px-3">
            <button
              type="button"
              onClick={openChat}
              aria-label="返回主 Agent"
              title="返回主 Agent"
              className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            >
              <ArrowLeft size={14} />
            </button>
            <span className="text-[var(--text-secondary)]">{workbench.icon}</span>
            <h1 className="text-sm font-medium text-[var(--text-primary)]">{workbench.label}</h1>
          </header>
          <div className="min-h-0 flex-1">{workbench.content}</div>
        </section>
      )}
    </div>
  );
}
