import { useEffect, useRef } from 'react';
import { FileCode, Globe2, GripVertical, ListTodo, PanelRightClose } from 'lucide-react';
import { useRightWorkspaceStore } from '../../stores/rightWorkspaceStore';
import { BrowserPanel } from '../browser/BrowserPanel';
import { FileBrowser } from '../file-browser/FileBrowser';
import { RightRail } from './RightRail';

export function RightWorkspace() {
  const store = useRightWorkspaceStore();
  const resizing = useRef(false);

  useEffect(() => {
    const move = (event: PointerEvent) => {
      if (resizing.current) store.setWidth(window.innerWidth - event.clientX);
    };
    const stop = () => {
      resizing.current = false;
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', stop);
    return () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', stop);
    };
  }, [store.setWidth]);

  if (!store.open) return null;

  return (
    <>
      <div className="fixed inset-0 z-[55] bg-black/25 lg:hidden" onClick={store.close} />
      <aside
        className="fixed inset-y-0 right-0 z-[60] flex w-[min(94vw,760px)] min-w-0 flex-col border-l border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-xl max-md:!w-full max-md:border-l-0 lg:relative lg:z-20 lg:shadow-none"
        style={{ width: `min(94vw, ${store.width}px)` }}
      >
        <button
          type="button"
          className="absolute inset-y-0 -left-1 z-10 hidden w-2 cursor-col-resize items-center justify-center text-transparent hover:text-[var(--text-tertiary)] lg:flex"
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
            active={store.activeTab === 'preview'}
            icon={<Globe2 size={13} />}
            label="预览"
            onClick={() => store.setActiveTab('preview')}
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

        {store.activeTab === 'preview' && (
          <div className="flex h-8 shrink-0 items-center gap-1 border-b border-[var(--border-primary)] px-2">
            <PreviewTabButton
              active={store.previewTab === 'browser'}
              icon={<Globe2 size={12} />}
              label="网页"
              onClick={() => store.setPreviewTab('browser')}
            />
            <PreviewTabButton
              active={store.previewTab === 'files'}
              icon={<FileCode size={12} />}
              label="文件"
              onClick={() => store.setPreviewTab('files')}
            />
          </div>
        )}

        <div className="min-h-0 flex-1">
          {store.activeTab === 'tasks' ? (
            <RightRail />
          ) : store.previewTab === 'browser' ? (
            <BrowserPanel />
          ) : (
            <FileBrowser />
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
      className={`flex h-7 items-center gap-1.5 rounded-md px-2.5 text-xs ${active ? 'bg-[var(--bg-hover)] text-[var(--text-primary)]' : 'text-[var(--text-tertiary)] hover:text-[var(--text-primary)]'}`}
    >
      {icon}
      {label}
    </button>
  );
}

function PreviewTabButton(props: TabButtonProps) {
  return <WorkspaceTab {...props} />;
}

interface TabButtonProps {
  active: boolean;
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
}
