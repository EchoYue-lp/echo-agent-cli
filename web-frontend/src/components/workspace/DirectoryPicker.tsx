import { useState, useEffect, useCallback } from 'react';
import { FolderOpen, ArrowUp, ChevronRight, X } from 'lucide-react';

interface BrowseEntry {
  name: string;
  path: string;
  is_dir: boolean;
}

interface BrowseResult {
  current: string;
  parent: string | null;
  entries: BrowseEntry[];
}

interface DirectoryPickerProps {
  isOpen: boolean;
  onClose: () => void;
  onSelect: (path: string) => void;
  initialPath?: string;
}

export default function DirectoryPicker({
  isOpen,
  onClose,
  onSelect,
  initialPath,
}: DirectoryPickerProps) {
  const [currentPath, setCurrentPath] = useState('');
  const [parentPath, setParentPath] = useState<string | null>(null);
  const [entries, setEntries] = useState<BrowseEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [selectedPath, setSelectedPath] = useState('');

  const browse = useCallback(async (path?: string) => {
    setLoading(true);
    try {
      const { get } = await import('../../api/client');
      const params = path ? `?path=${encodeURIComponent(path)}` : '';
      const data = await get<BrowseResult>(`/files/browse${params}`);
      setCurrentPath(data.current);
      setParentPath(data.parent);
      setEntries(data.entries);
      setSelectedPath(data.current);
    } catch (e) {
      console.error('Failed to browse:', e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (isOpen) {
      browse(initialPath);
    }
  }, [isOpen, initialPath, browse]);

  const handleGoUp = () => {
    if (parentPath) {
      browse(parentPath);
    }
  };

  const handleSelectDir = (entry: BrowseEntry) => {
    browse(entry.path);
  };

  const handleConfirm = () => {
    onSelect(selectedPath);
    onClose();
  };

  if (!isOpen) return null;

  return (
    <>
      {/* Backdrop */}
      <div className="fixed inset-0 z-[60] bg-black/50" onClick={onClose} />

      {/* Dialog */}
      <div
        className="fixed left-1/2 top-1/2 z-[60] flex w-[500px] -translate-x-1/2 -translate-y-1/2 flex-col overflow-hidden rounded-xl border shadow-2xl"
        style={{
          background: 'var(--bg-primary)',
          borderColor: 'var(--border-primary)',
          maxHeight: '70vh',
        }}
      >
        {/* Header */}
        <div
          className="flex items-center justify-between border-b px-4 py-3"
          style={{ borderColor: 'var(--border-primary)' }}
        >
          <div className="flex items-center gap-2">
            <FolderOpen size={16} style={{ color: 'var(--accent)' }} />
            <h3 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
              选择文件夹
            </h3>
          </div>
          <button
            onClick={onClose}
            className="rounded p-1 transition-colors hover:bg-[var(--bg-hover)]"
          >
            <X size={14} style={{ color: 'var(--text-tertiary)' }} />
          </button>
        </div>

        {/* Current path bar */}
        <div
          className="flex items-center gap-2 border-b px-4 py-2"
          style={{ borderColor: 'var(--border-primary)', background: 'var(--bg-secondary)' }}
        >
          <button
            onClick={handleGoUp}
            disabled={!parentPath || loading}
            className="flex items-center gap-1 rounded px-2 py-1 text-xs font-medium transition-colors hover:bg-[var(--bg-hover)] disabled:opacity-30"
            style={{ color: 'var(--text-secondary)' }}
          >
            <ArrowUp size={12} />
            上级
          </button>
          <div
            className="flex-1 truncate text-xs font-mono"
            style={{ color: 'var(--text-secondary)' }}
          >
            {currentPath}
          </div>
        </div>

        {/* Directory list */}
        <div className="flex-1 overflow-y-auto" style={{ minHeight: '250px' }}>
          {loading ? (
            <div
              className="flex items-center justify-center py-12"
              style={{ color: 'var(--text-tertiary)' }}
            >
              <span className="text-sm">加载中...</span>
            </div>
          ) : entries.length === 0 ? (
            <div
              className="flex flex-col items-center justify-center py-12"
              style={{ color: 'var(--text-tertiary)' }}
            >
              <FolderOpen size={32} className="mb-2 opacity-30" />
              <span className="text-sm">空目录</span>
            </div>
          ) : (
            <div className="py-1">
              {entries.map((entry) => (
                <button
                  key={entry.path}
                  onClick={() => handleSelectDir(entry)}
                  className="flex w-full items-center gap-2.5 px-4 py-2 text-left transition-colors hover:bg-[var(--bg-hover)]"
                >
                  <FolderOpen size={16} style={{ color: '#f59e0b' }} className="shrink-0" />
                  <span
                    className="flex-1 truncate text-sm"
                    style={{ color: 'var(--text-primary)' }}
                  >
                    {entry.name}
                  </span>
                  <ChevronRight
                    size={14}
                    style={{ color: 'var(--text-tertiary)' }}
                    className="shrink-0"
                  />
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Footer */}
        <div
          className="flex items-center justify-between border-t px-4 py-3"
          style={{ borderColor: 'var(--border-primary)' }}
        >
          <div className="flex-1 truncate text-xs" style={{ color: 'var(--text-tertiary)' }}>
            已选择: <span className="font-mono">{selectedPath}</span>
          </div>
          <div className="flex gap-2">
            <button
              onClick={onClose}
              className="rounded-lg px-3 py-1.5 text-xs font-medium transition-colors hover:bg-[var(--bg-hover)]"
              style={{ color: 'var(--text-secondary)' }}
            >
              取消
            </button>
            <button
              onClick={handleConfirm}
              className="rounded-lg px-4 py-1.5 text-xs font-medium text-[var(--text-on-accent)] transition-colors hover:opacity-90"
              style={{ background: 'var(--accent)' }}
            >
              选择此目录
            </button>
          </div>
        </div>
      </div>
    </>
  );
}
