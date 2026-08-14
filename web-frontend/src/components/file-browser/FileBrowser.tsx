import { useCallback, useEffect, useState } from 'react';
import CodeMirror from '@uiw/react-codemirror';
import { LanguageDescription } from '@codemirror/language';
import { languages } from '@codemirror/language-data';
import type { Extension } from '@codemirror/state';
import {
  AlertTriangle,
  Check,
  ChevronLeft,
  File as FileIcon,
  FileCode,
  FileDiff,
  FolderTree,
  GitCompare,
  Image as ImageIcon,
  Loader2,
  Pencil,
  RefreshCw,
  RotateCcw,
  Save,
  X,
} from 'lucide-react';
import { useUiStore } from '../../stores/uiStore';
import { useFileStore, type FileContent } from '../../stores/fileStore';
import { DiffViewer } from './DiffViewer';
import { FileTree } from './FileTree';

export function FileBrowser() {
  const store = useFileStore();
  const {
    loadChanges,
    loadTree,
    refreshSelectedFromDisk,
    saveSelected,
    selectFile: selectFileFromStore,
  } = store;
  const [mobileShowTree, setMobileShowTree] = useState(true);
  const selectedDocument = store.selectedFile ? store.documents[store.selectedFile] : undefined;

  useEffect(() => {
    void loadTree(4);
    void loadChanges();
  }, [loadChanges, loadTree]);

  useEffect(() => {
    const timer = window.setInterval(() => {
      void loadChanges();
      void refreshSelectedFromDisk();
    }, 2500);
    return () => window.clearInterval(timer);
  }, [loadChanges, refreshSelectedFromDisk]);

  useEffect(() => {
    const save = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 's') {
        event.preventDefault();
        void saveSelected();
      }
    };
    window.addEventListener('keydown', save);
    return () => window.removeEventListener('keydown', save);
  }, [saveSelected]);

  const selectFile = useCallback(
    (path: string) => {
      void selectFileFromStore(path);
      if (window.innerWidth < 768) setMobileShowTree(false);
    },
    [selectFileFromStore]
  );

  const closeFile = (path: string) => {
    if (store.closeFile(path)) return;
    if (window.confirm('该文件有未保存修改，确定放弃并关闭吗？')) {
      store.closeFile(path, true);
    }
  };

  const openChange = (path: string) => {
    void store.loadDiff(path);
    if (window.innerWidth < 768) setMobileShowTree(false);
  };

  return (
    <div className="relative flex h-full min-h-0 w-full">
      <div
        className={`w-[min(38%,220px)] min-w-[150px] shrink-0 overflow-hidden border-r border-[var(--border-primary)] bg-[var(--bg-sidebar)] max-md:absolute max-md:inset-y-0 max-md:left-0 max-md:z-20 max-md:w-[min(78vw,280px)] ${mobileShowTree ? '' : 'max-md:hidden'}`}
      >
        <div className="flex h-full flex-col">
          <div className="flex h-9 items-center justify-between border-b border-[var(--border-primary)] px-2.5">
            <div className="flex min-w-0 items-center gap-1.5">
              <FolderTree size={13} className="text-[var(--accent)]" />
              <span className="truncate text-xs font-medium text-[var(--text-secondary)]">
                项目文件
              </span>
            </div>
            <button
              type="button"
              onClick={() => void store.loadTree(4)}
              className="flex h-6 w-6 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
              title="刷新文件树"
            >
              <RefreshCw size={12} className={store.loading ? 'animate-spin' : ''} />
            </button>
          </div>
          {store.changes.length > 0 && (
            <div className="shrink-0 border-b border-[var(--border-primary)] px-1.5 py-1.5">
              <div className="flex h-7 items-center justify-between px-1.5 text-[10px] font-medium text-[var(--text-tertiary)]">
                <span className="flex items-center gap-1.5">
                  <FileDiff size={11} />
                  工作区变更
                </span>
                <span className="tabular-nums">{store.changes.length}</span>
              </div>
              <div className="max-h-40 space-y-0.5 overflow-y-auto">
                {store.changes.map((change) => (
                  <button
                    key={`${change.status}:${change.path}`}
                    type="button"
                    onClick={() => openChange(change.path)}
                    className="flex w-full items-center gap-2 rounded-md px-1.5 py-1 text-left hover:bg-[var(--bg-hover)]"
                  >
                    <span
                      className={`h-1.5 w-1.5 shrink-0 rounded-full ${
                        change.status === 'added'
                          ? 'bg-[var(--color-success)]'
                          : change.status === 'deleted'
                            ? 'bg-[var(--color-error)]'
                            : 'bg-[var(--color-warning)]'
                      }`}
                    />
                    <span className="min-w-0 flex-1 truncate font-mono text-[10px] text-[var(--text-secondary)]">
                      {change.path}
                    </span>
                  </button>
                ))}
              </div>
            </div>
          )}
          <div className="min-h-0 flex-1 overflow-y-auto">
            {store.loading && store.tree.length === 0 ? (
              <div className="flex justify-center py-8">
                <Loader2 size={18} className="animate-spin text-[var(--text-tertiary)]" />
              </div>
            ) : store.tree.length === 0 ? (
              <div className="px-4 py-8 text-center text-xs text-[var(--text-tertiary)]">
                未连接项目目录
              </div>
            ) : (
              <FileTree tree={store.tree} onSelect={selectFile} selectedFile={store.selectedFile} />
            )}
          </div>
        </div>
      </div>

      <div className="flex min-w-0 flex-1 flex-col">
        <FileTabs
          paths={store.openFiles}
          selected={store.selectedFile}
          documents={store.documents}
          onSelect={(path) => void store.selectFile(path)}
          onClose={closeFile}
        />

        {store.selectedFile && (selectedDocument || store.viewMode === 'diff') ? (
          <>
            <div className="flex h-9 shrink-0 items-center gap-1.5 border-b border-[var(--border-primary)] px-2">
              <button
                type="button"
                onClick={() => setMobileShowTree(true)}
                className="flex h-6 w-6 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] md:hidden"
                title="文件树"
              >
                <ChevronLeft size={13} />
              </button>
              <FileIcon size={12} className="shrink-0 text-[var(--text-tertiary)]" />
              <span className="min-w-0 flex-1 truncate text-[11px] text-[var(--text-secondary)]">
                {store.selectedFile}
              </span>
              <ModeButton
                active={store.viewMode === 'content'}
                icon={<FileCode size={12} />}
                label="预览"
                onClick={() => store.setViewMode('content')}
              />
              <ModeButton
                active={store.viewMode === 'edit'}
                disabled={!selectedDocument || selectedDocument.file.kind !== 'text'}
                icon={<Pencil size={12} />}
                label="编辑"
                onClick={() => store.setViewMode('edit')}
              />
              <ModeButton
                active={store.viewMode === 'diff'}
                icon={<GitCompare size={12} />}
                label="Diff"
                onClick={() => void store.loadDiff(store.selectedFile ?? '')}
              />
              {store.viewMode === 'edit' && selectedDocument && (
                <>
                  <button
                    type="button"
                    onClick={() => void store.discardSelected()}
                    disabled={!selectedDocument.dirty && !selectedDocument.conflict}
                    className="flex h-6 w-6 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] disabled:opacity-30"
                    title="恢复磁盘版本"
                  >
                    <RotateCcw size={12} />
                  </button>
                  <button
                    type="button"
                    onClick={() => void store.saveSelected()}
                    disabled={!selectedDocument.dirty || store.saving}
                    className="flex h-6 w-6 items-center justify-center rounded-md text-[var(--accent)] hover:bg-[var(--bg-hover)] disabled:opacity-30"
                    title="保存"
                  >
                    {store.saving ? (
                      <Loader2 size={12} className="animate-spin" />
                    ) : (
                      <Save size={12} />
                    )}
                  </button>
                </>
              )}
            </div>

            {selectedDocument?.conflict && (
              <div className="flex items-center gap-2 border-b border-[var(--color-warning)]/30 bg-[var(--color-warning)]/8 px-3 py-2 text-xs text-[var(--color-warning)]">
                <AlertTriangle size={13} />
                磁盘内容已变化。恢复磁盘版本后再编辑，或复制当前草稿后处理冲突。
              </div>
            )}

            {store.error && (
              <div className="flex items-center justify-between gap-3 border-b border-[var(--color-error)]/30 bg-[var(--color-error)]/5 px-3 py-2 text-xs text-[var(--color-error)]">
                <span className="truncate">{store.error}</span>
                <button type="button" onClick={store.clearError} title="关闭错误">
                  <X size={12} />
                </button>
              </div>
            )}

            <div className="min-h-0 flex-1 overflow-hidden">
              {store.loading ? (
                <div className="flex h-full items-center justify-center">
                  <Loader2 size={20} className="animate-spin text-[var(--text-tertiary)]" />
                </div>
              ) : store.viewMode === 'diff' ? (
                <DiffViewer hunks={store.diffHunks} path={store.selectedFile} />
              ) : store.viewMode === 'edit' ? (
                selectedDocument ? (
                  <CodeEditor
                    path={store.selectedFile}
                    value={selectedDocument.draft}
                    onChange={store.updateDraft}
                  />
                ) : null
              ) : selectedDocument ? (
                <FilePreview file={selectedDocument.file} />
              ) : null}
            </div>
          </>
        ) : (
          <div className="flex h-full flex-col items-center justify-center gap-2 text-[var(--text-tertiary)]">
            <FileCode size={28} strokeWidth={1.4} />
            <span className="text-xs">从文件树选择文件</span>
          </div>
        )}
      </div>
    </div>
  );
}

function FileTabs({
  paths,
  selected,
  documents,
  onSelect,
  onClose,
}: {
  paths: string[];
  selected: string | null;
  documents: ReturnType<typeof useFileStore.getState>['documents'];
  onSelect: (path: string) => void;
  onClose: (path: string) => void;
}) {
  if (paths.length === 0) return null;
  return (
    <div className="flex h-8 shrink-0 overflow-x-auto border-b border-[var(--border-primary)] bg-[var(--bg-primary)]">
      {paths.map((path) => {
        const name = path.split('/').pop() || path;
        const dirty = documents[path]?.dirty;
        return (
          <div
            key={path}
            className={`flex min-w-[110px] max-w-[190px] shrink-0 items-center border-r border-[var(--border-primary)] ${selected === path ? 'bg-[var(--bg-chat)]' : 'bg-[var(--bg-primary)]'}`}
          >
            <button
              type="button"
              className="flex min-w-0 flex-1 items-center gap-1.5 px-2 text-[11px] text-[var(--text-secondary)]"
              onClick={() => onSelect(path)}
              title={path}
            >
              {dirty ? (
                <span className="h-1.5 w-1.5 rounded-full bg-[var(--accent)]" />
              ) : (
                <Check size={10} />
              )}
              <span className="truncate">{name}</span>
            </button>
            <button
              type="button"
              className="mr-1 flex h-5 w-5 items-center justify-center rounded text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)]"
              onClick={() => onClose(path)}
              title="关闭文件"
            >
              <X size={10} />
            </button>
          </div>
        );
      })}
    </div>
  );
}

function ModeButton({
  active,
  disabled,
  icon,
  label,
  onClick,
}: {
  active: boolean;
  disabled?: boolean;
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className={`flex h-6 items-center gap-1 rounded-md px-1.5 text-[10px] transition-colors disabled:opacity-30 ${active ? 'bg-[var(--bg-hover)] text-[var(--text-primary)]' : 'text-[var(--text-tertiary)] hover:text-[var(--text-primary)]'}`}
    >
      {icon}
      {label}
    </button>
  );
}

function CodeEditor({
  path,
  value,
  onChange,
}: {
  path: string;
  value: string;
  onChange: (value: string) => void;
}) {
  const theme = useUiStore((state) => state.theme);
  const [extensions, setExtensions] = useState<Extension[]>([]);
  useEffect(() => {
    let disposed = false;
    const description = LanguageDescription.matchFilename(languages, path);
    if (!description) {
      setExtensions([]);
      return () => {
        disposed = true;
      };
    }
    void description.load().then((support) => {
      if (!disposed) setExtensions([support]);
    });
    return () => {
      disposed = true;
    };
  }, [path]);
  return (
    <CodeMirror
      value={value}
      height="100%"
      theme={theme}
      extensions={extensions}
      onChange={onChange}
      basicSetup={{ lineNumbers: true, foldGutter: true, highlightActiveLine: true }}
      className="h-full overflow-auto text-xs"
    />
  );
}

function FilePreview({ file }: { file: FileContent }) {
  if (file.kind === 'image' && file.data_url) {
    return (
      <div className="flex h-full items-center justify-center overflow-auto bg-[var(--bg-chat)] p-4">
        <img src={file.data_url} alt={file.path} className="max-h-full max-w-full object-contain" />
      </div>
    );
  }
  if (file.kind === 'pdf' && file.data_url) {
    return (
      <iframe src={file.data_url} title={file.path} className="h-full w-full border-0 bg-white" />
    );
  }
  if (file.kind === 'binary') {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-2 text-[var(--text-tertiary)]">
        <ImageIcon size={26} strokeWidth={1.4} />
        <span className="text-xs">该二进制文件不能在应用内预览</span>
      </div>
    );
  }
  const lines = file.content.split('\n');
  return (
    <div className="h-full overflow-auto font-mono text-xs leading-5">
      {lines.map((line, index) => (
        <div key={`${index}-${line.length}`} className="flex hover:bg-[var(--bg-hover)]">
          <span className="w-11 shrink-0 select-none px-2 text-right text-[var(--text-tertiary)]">
            {index + 1}
          </span>
          <span className="min-w-max flex-1 whitespace-pre px-2 text-[var(--text-primary)]">
            {line}
          </span>
        </div>
      ))}
    </div>
  );
}
