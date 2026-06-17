import { useEffect } from 'react';
import { X, FileWarning } from 'lucide-react';
import { useChangesStore } from '../../stores/changesStore';
import { useFileStore } from '../../stores/fileStore';
import { useChatStore } from '../../stores/chatStore';
import { DiffViewer } from '../file-browser/DiffViewer';

const STATUS_LABEL: Record<string, string> = {
  modified: '修改',
  added: '新增',
  deleted: '删除',
};
const STATUS_COLOR: Record<string, string> = {
  modified: 'var(--color-warning, #f59e0b)',
  added: 'var(--color-success, #22c55e)',
  deleted: 'var(--color-error, #ef4444)',
};

/** 从 args 里取出可展示的兜底内容(写入类工具的 content/new_string) */
function extractFallbackContent(args: unknown): string {
  if (!args || typeof args !== 'object') return '';
  const a = args as Record<string, unknown>;
  if (typeof a.content === 'string') return a.content;
  if (typeof a.new_string === 'string') return a.new_string;
  if (typeof a.text === 'string') return a.text;
  return JSON.stringify(a, null, 2);
}

export function ChangesDrawer() {
  const selectedPath = useChangesStore((s) => s.selectedPath);
  const setSelected = useChangesStore((s) => s.setSelected);
  const files = useChangesStore((s) => s.files);
  const isHistoryView = useChatStore((s) => s.isHistoryView);

  // fileStore 提供 loadDiff / diffHunks / loading / error
  const loadDiff = useFileStore((s) => s.loadDiff);
  const diffHunks = useFileStore((s) => s.diffHunks);
  const loading = useFileStore((s) => s.loading);
  const error = useFileStore((s) => s.error);

  const file = files.find((f) => f.path === selectedPath) || null;

  // 选中文件变化时拉取 diff
  useEffect(() => {
    if (!selectedPath) return;
    loadDiff(selectedPath);
  }, [selectedPath, loadDiff]);

  // Esc 关闭
  useEffect(() => {
    if (!selectedPath) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setSelected(null);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [selectedPath, setSelected]);

  if (!selectedPath || !file) return null;

  // 兜底触发条件:请求出错(非 git / 失败),或删除文件且 git diff 失败
  const showFallback = !!error;
  const fallbackContent = extractFallbackContent(file.lastWriteArgs);

  return (
    <div
      className="fixed inset-0 z-50 flex justify-end"
      onClick={() => setSelected(null)}
    >
      {/* 遮罩 */}
      <div className="absolute inset-0 bg-black/40" />
      {/* 抽屉 */}
      <div
        className="relative flex h-full w-[640px] max-w-[90vw] flex-col border-l border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        {/* 头部 */}
        <div className="flex items-start gap-3 border-b border-[var(--border-primary)] px-4 py-3">
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <span
                className="inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-medium"
                style={{
                  background: `color-mix(in srgb, ${STATUS_COLOR[file.status]} 18%, transparent)`,
                  color: STATUS_COLOR[file.status],
                }}
              >
                {STATUS_LABEL[file.status]}
              </span>
              <span className="truncate font-mono text-sm text-[var(--text-primary)]">
                {file.basename}
              </span>
            </div>
            {file.dir && (
              <div className="mt-0.5 truncate text-xs text-[var(--text-tertiary)]">
                {file.dir}
              </div>
            )}
          </div>
          <button
            onClick={() => setSelected(null)}
            className="rounded-md p-1 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            title="关闭"
          >
            <X size={16} />
          </button>
        </div>

        {/* 历史视图提示 */}
        {isHistoryView && (
          <div className="flex items-center gap-2 border-b border-[var(--border-primary)] px-4 py-2 text-xs text-[var(--color-warning, #f59e0b)]">
            <FileWarning size={13} className="shrink-0" />
            <span>显示的是当前工作区状态,非此历史会话发生时刻</span>
          </div>
        )}

        {/* 主体 */}
        <div className="min-h-0 flex-1 overflow-hidden">
          {loading ? (
            <div className="flex h-full items-center justify-center text-sm text-[var(--text-tertiary)]">
              加载 diff...
            </div>
          ) : showFallback ? (
            <FallbackView
              status={file.status}
              content={fallbackContent}
              onRetryGit={() => loadDiff(selectedPath)}
            />
          ) : (
            <DiffViewer hunks={diffHunks} path={file.path} />
          )}
        </div>
      </div>
    </div>
  );
}

function FallbackView({
  status,
  content,
  onRetryGit,
}: {
  status: string;
  content: string;
  onRetryGit: () => void;
}) {
  return (
    <div className="flex h-full flex-col">
      <div className="border-b border-[var(--border-primary)] px-4 py-2 text-xs text-[var(--text-tertiary)]">
        {status === 'deleted'
          ? '该文件已被删除,无法获取 git diff。以下为最近已知内容:'
          : '无法获取 git diff(可能为非 git 仓库)。以下为工具写入内容:'}
        <button
          onClick={onRetryGit}
          className="ml-3 rounded px-1.5 py-0.5 text-[var(--accent)] hover:underline"
        >
          重试 git diff
        </button>
      </div>
      <pre className="min-h-0 flex-1 overflow-auto bg-[var(--bg-code)] p-3 font-mono text-xs leading-relaxed text-[var(--color-code-text)]">
        {content || '(无可用内容)'}
      </pre>
    </div>
  );
}
