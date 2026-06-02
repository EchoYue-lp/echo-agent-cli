import { useEffect, useState } from 'react';
import { GitBranch, Plus, Trash2, Loader2, AlertCircle, Check, X, FolderOpen } from 'lucide-react';
import { worktreeApi, type WorktreeInfo } from '../../api/endpoints';

export function WorktreePanel() {
  const [worktrees, setWorktrees] = useState<WorktreeInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [showCreateForm, setShowCreateForm] = useState(false);
  const [newBranch, setNewBranch] = useState('');
  const [newBase, setNewBase] = useState('');
  const [creating, setCreating] = useState(false);

  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);

  const fetchWorktrees = async () => {
    try {
      setLoading(true);
      setError(null);
      const data = await worktreeApi.list();
      setWorktrees(data);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load worktrees');
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    fetchWorktrees();
  }, []);

  const handleCreate = async () => {
    if (!newBranch.trim()) return;
    setCreating(true);
    try {
      await worktreeApi.create({
        branch: newBranch.trim(),
        base: newBase.trim() || undefined,
      });
      setNewBranch('');
      setNewBase('');
      setShowCreateForm(false);
      await fetchWorktrees();
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to create worktree');
    } finally {
      setCreating(false);
    }
  };

  const handleRemove = async (branch: string) => {
    setRemoving(branch);
    try {
      await worktreeApi.remove(branch);
      setConfirmDelete(null);
      await fetchWorktrees();
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to remove worktree');
    } finally {
      setRemoving(null);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <Loader2 size={24} className="animate-spin text-[var(--text-tertiary)]" />
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <p className="text-sm text-[var(--text-secondary)]">
            Manage git worktrees for parallel development. Worktrees let you check out multiple
            branches simultaneously in separate directories.
          </p>
        </div>
        <button
          onClick={() => setShowCreateForm(true)}
          className="btn btn-primary flex items-center gap-2"
        >
          <Plus size={14} />
          New Worktree
        </button>
      </div>

      {/* Error banner */}
      {error && (
        <div className="flex items-center gap-2 rounded-lg border border-[var(--color-error)]/30 bg-[var(--color-error)]/5 px-4 py-3 text-sm text-[var(--color-error)]">
          <AlertCircle size={14} className="shrink-0" />
          <span>{error}</span>
          <button
            onClick={() => setError(null)}
            className="ml-auto rounded p-1 hover:bg-[var(--color-error)]/10"
          >
            <X size={12} />
          </button>
        </div>
      )}

      {/* Create form */}
      {showCreateForm && (
        <div className="rounded-xl border border-[var(--border-primary)] bg-[var(--bg-secondary)] p-4">
          <h3 className="mb-3 text-sm font-medium text-[var(--text-primary)]">
            Create New Worktree
          </h3>
          <div className="space-y-3">
            <div>
              <label className="mb-1 block text-xs font-medium text-[var(--text-secondary)]">
                Branch Name <span className="text-[var(--color-error)]">*</span>
              </label>
              <input
                value={newBranch}
                onChange={(e) => setNewBranch(e.target.value)}
                placeholder="feature/my-branch"
                className="input w-full"
                autoFocus
                onKeyDown={(e) => {
                  if (e.key === 'Enter') handleCreate();
                  if (e.key === 'Escape') setShowCreateForm(false);
                }}
              />
            </div>
            <div>
              <label className="mb-1 block text-xs font-medium text-[var(--text-secondary)]">
                Base (optional)
              </label>
              <input
                value={newBase}
                onChange={(e) => setNewBase(e.target.value)}
                placeholder="main, HEAD, or commit hash"
                className="input w-full"
                onKeyDown={(e) => {
                  if (e.key === 'Enter') handleCreate();
                  if (e.key === 'Escape') setShowCreateForm(false);
                }}
              />
              <p className="mt-1 text-[11px] text-[var(--text-tertiary)]">
                Leave empty to branch from HEAD. Specify a ref (e.g. main, v1.0) to branch from that
                point.
              </p>
            </div>
            <div className="flex items-center gap-2 pt-1">
              <button
                onClick={handleCreate}
                disabled={!newBranch.trim() || creating}
                className="btn btn-primary flex items-center gap-2 disabled:opacity-50"
              >
                {creating ? <Loader2 size={14} className="animate-spin" /> : <Check size={14} />}
                Create
              </button>
              <button
                onClick={() => {
                  setShowCreateForm(false);
                  setNewBranch('');
                  setNewBase('');
                }}
                className="btn flex items-center gap-2"
              >
                Cancel
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Worktree list */}
      {worktrees.length === 0 ? (
        <div className="rounded-xl border border-dashed border-[var(--border-primary)] p-8 text-center">
          <GitBranch size={32} className="mx-auto mb-3 text-[var(--text-tertiary)]" />
          <p className="text-sm text-[var(--text-secondary)]">No worktrees found</p>
          <p className="mt-1 text-xs text-[var(--text-tertiary)]">
            This directory may not be a git repository, or git is not installed.
          </p>
        </div>
      ) : (
        <div className="space-y-2">
          {worktrees.map((wt, idx) => {
            const isMain = idx === 0; // First worktree is typically the main one
            return (
              <div
                key={wt.path}
                className="group flex items-center gap-3 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] p-3 transition-colors hover:bg-[var(--bg-hover)]"
              >
                {/* Branch icon */}
                <div
                  className={`flex h-9 w-9 shrink-0 items-center justify-center rounded-lg ${
                    isMain
                      ? 'bg-[var(--accent)]/10 text-[var(--accent)]'
                      : 'bg-[var(--bg-secondary)] text-[var(--text-secondary)]'
                  }`}
                >
                  <GitBranch size={16} />
                </div>

                {/* Info */}
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="text-[13px] font-medium text-[var(--text-primary)]">
                      {wt.branch || '(no branch)'}
                    </span>
                    {isMain && (
                      <span className="rounded-full bg-[var(--accent)]/10 px-2 py-0.5 text-[10px] font-medium text-[var(--accent)]">
                        main
                      </span>
                    )}
                    {wt.managed && (
                      <span className="rounded-full bg-[var(--color-success)]/10 px-2 py-0.5 text-[10px] font-medium text-[var(--color-success)]">
                        managed
                      </span>
                    )}
                  </div>
                  <div className="mt-0.5 flex items-center gap-2 text-[11px] text-[var(--text-tertiary)]">
                    <FolderOpen size={10} />
                    <span className="truncate font-mono">{wt.path}</span>
                    {wt.head && (
                      <>
                        <span>·</span>
                        <span className="font-mono">{wt.head}</span>
                      </>
                    )}
                  </div>
                </div>

                {/* Actions */}
                {!isMain && (
                  <div className="flex shrink-0 items-center gap-1 opacity-0 transition-opacity group-hover:opacity-100">
                    {confirmDelete === wt.branch ? (
                      <div className="flex items-center gap-1">
                        <button
                          onClick={() => handleRemove(wt.branch)}
                          disabled={removing === wt.branch}
                          className="rounded bg-[var(--color-error)] px-2 py-1 text-[11px] font-medium text-white hover:opacity-90 disabled:opacity-50"
                        >
                          {removing === wt.branch ? (
                            <Loader2 size={11} className="animate-spin" />
                          ) : (
                            'Confirm'
                          )}
                        </button>
                        <button
                          onClick={() => setConfirmDelete(null)}
                          className="rounded px-2 py-1 text-[11px] text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
                        >
                          Cancel
                        </button>
                      </div>
                    ) : (
                      <button
                        onClick={() => setConfirmDelete(wt.branch)}
                        className="rounded p-1.5 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--color-error)]/10 hover:text-[var(--color-error)]"
                        title="Remove worktree"
                      >
                        <Trash2 size={13} />
                      </button>
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
