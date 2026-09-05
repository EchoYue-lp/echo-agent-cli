import { useEffect, useRef, useState } from 'react';
import { ArrowLeft, GripVertical, PanelRightClose } from 'lucide-react';
import { contextPaneWidthForViewport, useContextPaneStore } from '../../stores/contextPaneStore';
import { useUiStore } from '../../stores/uiStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useConversationStore } from '../../stores/conversationStore';
import { subagentRunStoreKey, useSubagentRunStore } from '../../stores/subagentRunStore';
import { RightRail } from './RightRail';
import { SubagentDetailView } from '../task/SubagentDetailView';
import { BrowserPanel } from '../browser/BrowserPanel';
import { FileBrowser } from '../file-browser/FileBrowser';
import { productDataScope, productDataScopeKey } from '../../lib/productDataScope';

export function ContextPane() {
  const target = useContextPaneStore((state) => state.target);
  const returnTarget = useContextPaneStore((state) => state.returnTarget);
  const widthPreference = useContextPaneStore((state) => state.width);
  const close = useContextPaneStore((state) => state.close);
  const setWidth = useContextPaneStore((state) => state.setWidth);
  const leftSidebarOpen = useUiStore((state) => state.leftSidebarOpen);
  const workspace = useWorkspaceStore((state) => state.current);
  const activeConversationId = useConversationStore((state) => state.activeId);
  const workspaceScope = productDataScopeKey(productDataScope(workspace));
  const runs = useSubagentRunStore((state) => state.runs);
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

  useEffect(() => {
    useContextPaneStore.getState().reset();
  }, [workspace?.id, activeConversationId]);

  useEffect(() => {
    if (!target) return;
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') close();
    };
    window.addEventListener('keydown', handleEscape);
    return () => window.removeEventListener('keydown', handleEscape);
  }, [close, target]);

  if (!target) return null;

  const width = contextPaneWidthForViewport(widthPreference, viewportWidth, leftSidebarOpen);
  const maxWidth = contextPaneWidthForViewport(760, viewportWidth, leftSidebarOpen);
  const selectedSubagent =
    target.kind === 'subagent'
      ? runs[subagentRunStoreKey(target.runId, target.subagentRunId)]
      : undefined;
  const title =
    target.kind === 'subagent'
      ? (selectedSubagent?.agent ?? 'Subagent')
      : target.kind === 'tasks'
        ? '任务运行'
        : target.kind === 'browser'
          ? '浏览器'
          : '文件';

  return (
    <>
      <button
        type="button"
        aria-label="点击遮罩关闭上下文面板"
        className="fixed inset-0 z-[55] bg-black/25 xl:hidden"
        onClick={close}
      />
      <aside
        className="fixed inset-y-0 right-0 z-[60] flex min-w-0 flex-col border-l border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-xl max-md:!w-full max-md:border-l-0 xl:relative xl:z-20 xl:shadow-none"
        style={{ width }}
        aria-label={`${title}上下文面板`}
      >
        <button
          type="button"
          aria-label="调整上下文面板宽度"
          title="调整上下文面板宽度"
          className="absolute inset-y-0 -left-1 z-10 hidden w-2 cursor-col-resize items-center justify-center text-transparent hover:text-[var(--text-tertiary)] xl:flex"
          onPointerDown={() => {
            resizing.current = true;
          }}
          onKeyDown={(event) => {
            if (event.key === 'ArrowLeft') {
              event.preventDefault();
              setWidth(width + 24);
            } else if (event.key === 'ArrowRight') {
              event.preventDefault();
              setWidth(width - 24);
            } else if (event.key === 'Home') {
              event.preventDefault();
              setWidth(380);
            } else if (event.key === 'End') {
              event.preventDefault();
              setWidth(maxWidth);
            }
          }}
        >
          <GripVertical size={12} />
        </button>
        {target.kind === 'subagent' ? (
          selectedSubagent ? (
            <SubagentDetailView
              key={`${selectedSubagent.runId}\u0000${selectedSubagent.subagentRunId}`}
              run={selectedSubagent}
              onBack={close}
            />
          ) : (
            <MissingContext text="Subagent 执行记录已不可用" onClose={close} />
          )
        ) : (
          <>
            <header className="flex h-11 shrink-0 items-center gap-2 border-b border-[var(--border-primary)] px-3">
              <div className="min-w-0 flex-1 truncate text-sm font-medium text-[var(--text-primary)]">
                {title}
              </div>
              <button
                type="button"
                onClick={close}
                autoFocus
                aria-label={returnTarget ? '返回 Subagent' : '关闭上下文面板'}
                title={returnTarget ? '返回 Subagent' : '关闭上下文面板'}
                className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
              >
                {returnTarget ? <ArrowLeft size={14} /> : <PanelRightClose size={14} />}
              </button>
            </header>
            <div className="min-h-0 flex-1">
              {target.kind === 'tasks' ? (
                <RightRail />
              ) : target.kind === 'browser' ? (
                <BrowserPanel />
              ) : (
                <FileBrowser key={workspaceScope} />
              )}
            </div>
          </>
        )}
      </aside>
    </>
  );
}

function MissingContext({ text, onClose }: { text: string; onClose: () => void }) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 px-6 text-center text-xs text-[var(--text-tertiary)]">
      <span>{text}</span>
      <button
        type="button"
        onClick={onClose}
        aria-label="关闭不可用的上下文"
        title="关闭上下文"
        className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
      >
        <PanelRightClose size={14} />
      </button>
    </div>
  );
}
