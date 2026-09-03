import { useEffect, useRef, useState, type ReactNode } from 'react';
import {
  BookOpen,
  FileCode,
  FlaskConical,
  Globe2,
  GripVertical,
  ListTodo,
  PanelRightClose,
  Workflow,
} from 'lucide-react';
import {
  rightWorkspaceWidthForViewport,
  useRightWorkspaceStore,
} from '../../stores/rightWorkspaceStore';
import { useUiStore } from '../../stores/uiStore';
import { BrowserPanel } from '../browser/BrowserPanel';
import { FileBrowser } from '../file-browser/FileBrowser';
import AnalysisPanel from '../analysis/AnalysisPanel';
import { PaperPanel } from '../papers/PaperPanel';
import { AutomationPanel } from '../automation/AutomationPanel';
import { RightRail } from './RightRail';
import { SubagentDetailView } from '../task/SubagentDetailView';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { subagentRunStoreKey, useSubagentRunStore } from '../../stores/subagentRunStore';
import { useSubagentDetailStore } from '../../stores/subagentDetailStore';
import { productDataScope, productDataScopeKey } from '../../lib/productDataScope';

export function RightWorkspace() {
  const store = useRightWorkspaceStore();
  const setWidth = store.setWidth;
  const leftSidebarOpen = useUiStore((state) => state.leftSidebarOpen);
  const workspace = useWorkspaceStore((state) => state.current);
  const productScopeKey = productDataScopeKey(productDataScope(workspace));
  const resizing = useRef(false);
  const [viewportWidth, setViewportWidth] = useState(() => window.innerWidth);

  useEffect(() => {
    const move = (event: PointerEvent) => {
      if (resizing.current) setWidth(window.innerWidth - event.clientX);
    };
    const stop = () => {
      resizing.current = false;
    };
    const resize = () => setViewportWidth(window.innerWidth);
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', stop);
    window.addEventListener('resize', resize);
    return () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', stop);
      window.removeEventListener('resize', resize);
    };
  }, [setWidth]);

  // Subagent detail takes over the panel body (navigation-stack semantics):
  // back returns to the previously active tab. The tab header stays clickable.
  const selectedSubagentRef = useSubagentDetailStore((s) => s.selected);
  const closeSubagentDetail = useSubagentDetailStore((s) => s.close);
  const subagentRuns = useSubagentRunStore((s) => s.runs);
  const selectedSubagent = selectedSubagentRef
    ? subagentRuns[
        subagentRunStoreKey(selectedSubagentRef.runId, selectedSubagentRef.subagentRunId)
      ]
    : undefined;

  if (!store.open) return null;

  const width = rightWorkspaceWidthForViewport(store.width, viewportWidth, leftSidebarOpen);

  return (
    <>
      <button
        type="button"
        aria-label="关闭工作区面板"
        className="fixed inset-0 z-[55] bg-black/25 xl:hidden"
        onClick={store.close}
      />
      <aside
        className="fixed inset-y-0 right-0 z-[60] flex min-w-0 flex-col border-l border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-xl max-md:!w-full max-md:border-l-0 xl:relative xl:z-20 xl:shadow-none"
        style={{ width }}
      >
        <button
          type="button"
          className="absolute inset-y-0 -left-1 z-10 hidden w-2 cursor-col-resize items-center justify-center text-transparent hover:text-[var(--text-tertiary)] xl:flex"
          onPointerDown={() => {
            resizing.current = true;
          }}
          title="调整宽度"
        >
          <GripVertical size={12} />
        </button>

        <header className="flex h-10 shrink-0 items-center border-b border-[var(--border-primary)] px-2">
          <WorkspaceTab
            active={store.activeTab === 'tasks'}
            icon={<ListTodo size={13} />}
            label="任务"
            onClick={() => store.setActiveTab('tasks')}
          />
          <WorkspaceTab
            active={store.activeTab === 'analysis'}
            icon={<FlaskConical size={13} />}
            label="分析"
            onClick={() => store.setActiveTab('analysis')}
          />
          <WorkspaceTab
            active={store.activeTab === 'research'}
            icon={<BookOpen size={13} />}
            label="研究"
            onClick={() => store.setActiveTab('research')}
          />
          <WorkspaceTab
            active={store.activeTab === 'browser'}
            icon={<Globe2 size={13} />}
            label="浏览器"
            onClick={() => store.setActiveTab('browser')}
          />
          <WorkspaceTab
            active={store.activeTab === 'files'}
            icon={<FileCode size={13} />}
            label="文件"
            onClick={() => store.setActiveTab('files')}
          />
          <WorkspaceTab
            active={store.activeTab === 'automation'}
            icon={<Workflow size={13} />}
            label="自动化"
            onClick={() => store.setActiveTab('automation')}
          />
          <div className="flex-1" />
          <button
            type="button"
            onClick={store.close}
            className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            title="收起右侧工作区"
          >
            <PanelRightClose size={14} />
          </button>
        </header>

        <div className="min-h-0 flex-1">
          {selectedSubagent ? (
            <SubagentDetailView run={selectedSubagent} onBack={closeSubagentDetail} />
          ) : store.activeTab === 'tasks' ? (
            <RightRail />
          ) : store.activeTab === 'analysis' ? (
            <AnalysisPanel key={productScopeKey} />
          ) : store.activeTab === 'research' ? (
            <PaperPanel key={productScopeKey} />
          ) : store.activeTab === 'browser' ? (
            <BrowserPanel />
          ) : store.activeTab === 'automation' ? (
            <AutomationPanel />
          ) : (
            <FileBrowser key={productScopeKey} />
          )}
        </div>
      </aside>
    </>
  );
}

function WorkspaceTab({ active, icon, label, onClick }: TabButtonProps) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      className={`relative flex h-10 items-center gap-1.5 px-2.5 text-xs transition-colors ${active ? 'text-[var(--text-primary)] after:absolute after:inset-x-2 after:bottom-0 after:h-0.5 after:bg-[var(--accent)]' : 'text-[var(--text-tertiary)] hover:text-[var(--text-primary)]'}`}
    >
      {icon}
      {label}
    </button>
  );
}

interface TabButtonProps {
  active: boolean;
  icon: ReactNode;
  label: string;
  onClick: () => void;
}
