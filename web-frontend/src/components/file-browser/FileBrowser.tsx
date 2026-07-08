import { useEffect, useState, useCallback } from 'react';
import { useFileStore } from '../../stores/fileStore';
import { FileTree } from './FileTree';
import { DiffViewer } from './DiffViewer';
import {
  FolderTree,
  RefreshCw,
  FileCode,
  GitCompare,
  X,
  Loader2,
  File as FileIcon,
  ChevronLeft,
} from 'lucide-react';

export function FileBrowser() {
  const {
    tree,
    selectedFile,
    fileContent,
    diffHunks,
    loading,
    error,
    viewMode,
    loadTree,
    selectFile,
    loadDiff,
    clearSelection,
  } = useFileStore();

  const [sidebarWidth] = useState(260);
  const [mobileShowTree, setMobileShowTree] = useState(true);

  useEffect(() => {
    loadTree(3);
  }, [loadTree]);

  const handleRefresh = useCallback(() => {
    loadTree(3);
  }, [loadTree]);

  const handleSelectFile = useCallback(
    (path: string) => {
      selectFile(path);
      // On mobile, hide tree after selection
      if (window.innerWidth < 768) {
        setMobileShowTree(false);
      }
    },
    [selectFile]
  );

  const handleViewDiff = useCallback(() => {
    if (selectedFile) {
      loadDiff(selectedFile);
    }
  }, [selectedFile, loadDiff]);

  const handleViewContent = useCallback(() => {
    if (selectedFile) {
      selectFile(selectedFile);
    }
  }, [selectedFile, selectFile]);

  const handleBackToTree = useCallback(() => {
    setMobileShowTree(true);
  }, []);

  return (
    <div className="flex h-full w-full min-h-0">
      {/* Left panel: file tree */}
      <div
        className={`shrink-0 overflow-hidden border-r border-[var(--border-primary)] bg-[var(--bg-sidebar)] transition-all duration-200
          ${mobileShowTree ? 'max-md:fixed max-md:inset-y-0 max-md:left-0 max-md:z-30 max-md:w-[260px]' : 'max-md:hidden'}
          ${!selectedFile || mobileShowTree ? '' : 'max-md:hidden'}`}
        style={{ width: sidebarWidth }}
      >
        <div className="flex h-full flex-col">
          {/* Tree header */}
          <div className="flex items-center justify-between border-b border-[var(--border-primary)] px-3 py-2.5">
            <div className="flex items-center gap-2">
              <FolderTree size={14} className="text-[var(--accent)]" />
              <span className="text-xs font-semibold uppercase tracking-wider text-[var(--text-secondary)]">
                Files
              </span>
            </div>
            <button
              onClick={handleRefresh}
              className="rounded-md p-1 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-sidebar-hover)] hover:text-[var(--text-primary)]"
              title="Refresh"
            >
              <RefreshCw size={12} className={loading ? 'animate-spin' : ''} />
            </button>
          </div>

          {/* Tree content */}
          <div className="flex-1 overflow-y-auto">
            {loading && tree.length === 0 ? (
              <div className="flex items-center justify-center py-8">
                <Loader2 size={20} className="animate-spin text-[var(--text-tertiary)]" />
              </div>
            ) : tree.length === 0 ? (
              <div className="px-4 py-8 text-center">
                <FolderTree size={24} className="mx-auto mb-2 text-[var(--text-tertiary)]" />
                <p className="text-xs text-[var(--text-tertiary)]">No workspace linked</p>
                <p className="mt-1 text-[10px] text-[var(--text-tertiary)]">
                  Link a project to browse files
                </p>
              </div>
            ) : (
              <FileTree tree={tree} onSelect={handleSelectFile} selectedFile={selectedFile} />
            )}
          </div>
        </div>
      </div>

      {/* Right panel: file content or diff */}
      <div className="flex min-w-0 flex-1 flex-col min-h-0">
        {selectedFile ? (
          <>
            {/* Toolbar */}
            <div className="flex items-center gap-2 border-b border-[var(--border-primary)] px-4 py-2">
              {/* Mobile back button */}
              <button
                onClick={handleBackToTree}
                className="mr-1 rounded-md p-1 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] md:hidden"
              >
                <ChevronLeft size={14} />
              </button>

              {/* File name */}
              <FileIcon size={13} className="shrink-0 text-[var(--text-tertiary)]" />
              <span className="truncate text-xs font-medium text-[var(--text-primary)]">
                {selectedFile}
              </span>

              <div className="flex-1" />

              {/* View mode toggle */}
              <div className="flex items-center gap-0.5 rounded-lg border border-[var(--border-primary)] p-0.5">
                <button
                  onClick={handleViewContent}
                  className={`flex items-center gap-1 rounded-md px-2 py-1 text-[10px] font-medium transition-colors
                    ${
                      viewMode === 'content'
                        ? 'bg-[var(--bg-sidebar-active)] text-[var(--text-primary)]'
                        : 'text-[var(--text-tertiary)] hover:text-[var(--text-primary)]'
                    }`}
                >
                  <FileCode size={10} />
                  Content
                </button>
                <button
                  onClick={handleViewDiff}
                  className={`flex items-center gap-1 rounded-md px-2 py-1 text-[10px] font-medium transition-colors
                    ${
                      viewMode === 'diff'
                        ? 'bg-[var(--bg-sidebar-active)] text-[var(--text-primary)]'
                        : 'text-[var(--text-tertiary)] hover:text-[var(--text-primary)]'
                    }`}
                >
                  <GitCompare size={10} />
                  Diff
                </button>
              </div>

              {/* Close button */}
              <button
                onClick={clearSelection}
                className="rounded-md p-1 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                title="Close"
              >
                <X size={12} />
              </button>
            </div>

            {/* Content area */}
            <div className="flex-1 overflow-hidden min-h-0">
              {loading ? (
                <div className="flex h-full items-center justify-center">
                  <Loader2 size={24} className="animate-spin text-[var(--text-tertiary)]" />
                </div>
              ) : error ? (
                <div className="flex h-full items-center justify-center px-4 text-center">
                  <div>
                    <div className="mb-2 text-sm font-medium text-[var(--color-error)]">
                      Error loading file
                    </div>
                    <div className="text-xs text-[var(--text-tertiary)]">{error}</div>
                  </div>
                </div>
              ) : viewMode === 'diff' ? (
                <DiffViewer hunks={diffHunks} path={selectedFile} />
              ) : fileContent ? (
                <FileContentView content={fileContent} />
              ) : null}
            </div>
          </>
        ) : (
          /* Empty state */
          <div className="flex h-full items-center justify-center">
            <div className="text-center">
              <FileCode size={36} className="mx-auto mb-3 text-[var(--text-tertiary)]" />
              <p className="text-sm font-medium text-[var(--text-secondary)]">
                Select a file to view
              </p>
              <p className="mt-1 text-xs text-[var(--text-tertiary)]">
                Browse the file tree or view git diffs
              </p>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

/* ── Inline file content viewer ─────────────────────────────────── */

function FileContentView({
  content,
}: {
  content: { path: string; content: string; size: number; language?: string };
}) {
  const lines = content.content.split('\n');

  return (
    <div className="flex h-full flex-col min-h-0">
      {/* Info bar */}
      <div className="flex items-center gap-3 border-b border-[var(--border-primary)] px-4 py-1.5 text-[10px] text-[var(--text-tertiary)]">
        {content.language && (
          <span className="rounded-md bg-[var(--bg-hover)] px-1.5 py-0.5 font-medium uppercase">
            {content.language}
          </span>
        )}
        <span>{lines.length} lines</span>
        <span>{formatSize(content.size)}</span>
      </div>

      {/* Code content */}
      <div className="flex-1 overflow-auto font-mono text-xs leading-5">
        {lines.map((line, idx) => (
          <div key={idx} className="flex hover:bg-[var(--bg-hover)]">
            <span className="w-12 shrink-0 select-none px-2 text-right text-[var(--text-tertiary)]">
              {idx + 1}
            </span>
            <span className="flex-1 whitespace-pre px-3 text-[var(--text-primary)]">{line}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
