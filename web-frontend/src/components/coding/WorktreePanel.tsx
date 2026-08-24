import { useCallback, useEffect, useRef, useState } from 'react';
import {
  AlertCircle,
  Check,
  FolderOpen,
  GitBranch,
  GitMerge,
  Loader2,
  Plus,
  RotateCcw,
  Trash2,
  Unlock,
  X,
} from 'lucide-react';
import { worktreeApi, type UnattendedWorktreeInfo, type WorktreeInfo } from '../../api/endpoints';
import { workspaceIdForView } from '../../lib/viewAddress';
import { useWorkspaceStore } from '../../stores/workspaceStore';

type ReviewAction = 'merge' | 'discard';

export function WorktreePanel() {
  const workspaceId = useWorkspaceStore((state) => workspaceIdForView(state.current?.id));
  const scopeRef = useRef({ workspaceId, generation: 0 });
  if (scopeRef.current.workspaceId !== workspaceId) {
    scopeRef.current = { workspaceId, generation: scopeRef.current.generation + 1 };
  }
  const scopeGeneration = scopeRef.current.generation;
  const fetchRequestRef = useRef(0);
  const actionRequestRef = useRef(0);
  const [worktrees, setWorktrees] = useState<WorktreeInfo[]>([]);
  const [unattended, setUnattended] = useState<UnattendedWorktreeInfo[]>([]);
  const [dataScopeGeneration, setDataScopeGeneration] = useState(-1);
  const [loading, setLoading] = useState(true);
  const [loadingScopeGeneration, setLoadingScopeGeneration] = useState(-1);
  const [error, setError] = useState<string | null>(null);
  const [errorScopeGeneration, setErrorScopeGeneration] = useState(-1);
  const [showCreateForm, setShowCreateForm] = useState(false);
  const [newBranch, setNewBranch] = useState('');
  const [newBase, setNewBase] = useState('');
  const [creating, setCreating] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const [reviewAction, setReviewAction] = useState<{ runId: string; action: ReviewAction } | null>(
    null
  );
  const [reviewing, setReviewing] = useState<string | null>(null);
  const [cleaning, setCleaning] = useState(false);
  const [actionScopeGeneration, setActionScopeGeneration] = useState(-1);
  const [reviewActionScopeGeneration, setReviewActionScopeGeneration] = useState(-1);

  const requestIsCurrent = useCallback(
    (requestWorkspaceId: string, requestGeneration: number) =>
      scopeRef.current.workspaceId === requestWorkspaceId &&
      scopeRef.current.generation === requestGeneration &&
      workspaceIdForView(useWorkspaceStore.getState().current?.id) === requestWorkspaceId,
    []
  );
  const visibleError = errorScopeGeneration === scopeGeneration ? error : null;
  const visibleCreating = actionScopeGeneration === scopeGeneration ? creating : false;
  const visibleRemoving = actionScopeGeneration === scopeGeneration ? removing : null;
  const visibleReviewing = actionScopeGeneration === scopeGeneration ? reviewing : null;
  const visibleCleaning = actionScopeGeneration === scopeGeneration ? cleaning : false;
  const visibleReviewAction = reviewActionScopeGeneration === scopeGeneration ? reviewAction : null;
  const chooseReviewAction = (runId: string, action: ReviewAction) => {
    setReviewActionScopeGeneration(scopeGeneration);
    setReviewAction({ runId, action });
  };

  const fetchWorktrees = useCallback(async () => {
    const requestGeneration = scopeRef.current.generation;
    const requestToken = fetchRequestRef.current + 1;
    fetchRequestRef.current = requestToken;
    try {
      setLoadingScopeGeneration(requestGeneration);
      setLoading(true);
      setErrorScopeGeneration(requestGeneration);
      setError(null);
      const [allWorktrees, unattendedWorktrees] = await Promise.all([
        worktreeApi.list(workspaceId),
        worktreeApi.listUnattended(workspaceId),
      ]);
      if (
        !requestIsCurrent(workspaceId, requestGeneration) ||
        fetchRequestRef.current !== requestToken
      )
        return;
      setDataScopeGeneration(requestGeneration);
      setWorktrees(allWorktrees);
      setUnattended(unattendedWorktrees);
    } catch (e) {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        fetchRequestRef.current === requestToken
      ) {
        setDataScopeGeneration(requestGeneration);
        setWorktrees([]);
        setUnattended([]);
        setErrorScopeGeneration(requestGeneration);
        setError(e instanceof Error ? e.message : 'Failed to load worktrees');
      }
    } finally {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        fetchRequestRef.current === requestToken
      ) {
        setLoading(false);
      }
    }
  }, [requestIsCurrent, workspaceId]);

  useEffect(() => {
    void fetchWorktrees();
  }, [fetchWorktrees]);

  const handleCreate = async () => {
    if (!newBranch.trim()) return;
    const requestGeneration = scopeRef.current.generation;
    const requestToken = actionRequestRef.current + 1;
    actionRequestRef.current = requestToken;
    setActionScopeGeneration(requestGeneration);
    setRemoving(null);
    setReviewing(null);
    setCleaning(false);
    setCreating(true);
    try {
      await worktreeApi.create(workspaceId, {
        branch: newBranch.trim(),
        base: newBase.trim() || undefined,
      });
      if (
        !requestIsCurrent(workspaceId, requestGeneration) ||
        actionRequestRef.current !== requestToken
      )
        return;
      setNewBranch('');
      setNewBase('');
      setShowCreateForm(false);
      await fetchWorktrees();
    } catch (e) {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        actionRequestRef.current === requestToken
      ) {
        setErrorScopeGeneration(requestGeneration);
        setError(e instanceof Error ? e.message : 'Failed to create worktree');
      }
    } finally {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        actionRequestRef.current === requestToken
      ) {
        setCreating(false);
      }
    }
  };

  const handleRemove = async (path: string) => {
    const requestGeneration = scopeRef.current.generation;
    const requestToken = actionRequestRef.current + 1;
    actionRequestRef.current = requestToken;
    setActionScopeGeneration(requestGeneration);
    setCreating(false);
    setReviewing(null);
    setCleaning(false);
    setRemoving(path);
    try {
      await worktreeApi.remove(workspaceId, path);
      if (
        !requestIsCurrent(workspaceId, requestGeneration) ||
        actionRequestRef.current !== requestToken
      )
        return;
      setConfirmDelete(null);
      await fetchWorktrees();
    } catch (e) {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        actionRequestRef.current === requestToken
      ) {
        setErrorScopeGeneration(requestGeneration);
        setError(e instanceof Error ? e.message : 'Failed to remove worktree');
      }
    } finally {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        actionRequestRef.current === requestToken
      ) {
        setRemoving(null);
      }
    }
  };

  const handleReviewAction = async (runId: string, action: ReviewAction) => {
    const requestGeneration = scopeRef.current.generation;
    const requestToken = actionRequestRef.current + 1;
    actionRequestRef.current = requestToken;
    setActionScopeGeneration(requestGeneration);
    setCreating(false);
    setRemoving(null);
    setCleaning(false);
    setReviewing(runId);
    try {
      if (action === 'merge') {
        const result = await worktreeApi.mergeUnattended(workspaceId, runId);
        if (
          !requestIsCurrent(workspaceId, requestGeneration) ||
          actionRequestRef.current !== requestToken
        )
          return;
        if (result.cleanup_warning) {
          setErrorScopeGeneration(requestGeneration);
          setError(result.cleanup_warning);
        }
      } else {
        await worktreeApi.discardUnattended(workspaceId, runId);
      }
      if (
        !requestIsCurrent(workspaceId, requestGeneration) ||
        actionRequestRef.current !== requestToken
      )
        return;
      setReviewAction(null);
      await fetchWorktrees();
    } catch (e) {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        actionRequestRef.current === requestToken
      ) {
        setErrorScopeGeneration(requestGeneration);
        setError(e instanceof Error ? e.message : `Failed to ${action} worktree`);
      }
    } finally {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        actionRequestRef.current === requestToken
      ) {
        setReviewing(null);
      }
    }
  };

  const handleCleanup = async () => {
    const requestGeneration = scopeRef.current.generation;
    const requestToken = actionRequestRef.current + 1;
    actionRequestRef.current = requestToken;
    setActionScopeGeneration(requestGeneration);
    setCreating(false);
    setRemoving(null);
    setReviewing(null);
    setCleaning(true);
    try {
      const result = await worktreeApi.cleanupUnattended(workspaceId);
      if (
        !requestIsCurrent(workspaceId, requestGeneration) ||
        actionRequestRef.current !== requestToken
      )
        return;
      if (result.errors.length > 0) {
        setErrorScopeGeneration(requestGeneration);
        setError(result.errors.join('\n'));
      }
      await fetchWorktrees();
    } catch (e) {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        actionRequestRef.current === requestToken
      ) {
        setErrorScopeGeneration(requestGeneration);
        setError(e instanceof Error ? e.message : 'Failed to clean worktrees');
      }
    } finally {
      if (
        requestIsCurrent(workspaceId, requestGeneration) &&
        actionRequestRef.current === requestToken
      ) {
        setCleaning(false);
      }
    }
  };

  const visibleLoading = loadingScopeGeneration === scopeGeneration ? loading : true;
  const visibleUnattended = dataScopeGeneration === scopeGeneration ? unattended : [];
  const scopedWorktrees = dataScopeGeneration === scopeGeneration ? worktrees : [];

  if (visibleLoading) {
    return (
      <div className="flex items-center justify-center py-12">
        <Loader2 size={24} className="animate-spin text-[var(--text-tertiary)]" />
      </div>
    );
  }

  const cleanableCount = visibleUnattended.filter(
    (item) => !item.active && !item.has_changes
  ).length;
  const visibleWorktrees = scopedWorktrees.filter(
    (worktree) => !worktree.branch.startsWith('eko-unattended-')
  );

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-end">
        <button
          onClick={() => setShowCreateForm(true)}
          className="btn btn-primary flex items-center gap-2"
        >
          <Plus size={14} />
          New Worktree
        </button>
      </div>

      {visibleError && (
        <div className="flex items-start gap-2 rounded-lg border border-[var(--color-error)]/30 bg-[var(--color-error)]/5 px-4 py-3 text-sm text-[var(--color-error)]">
          <AlertCircle size={14} className="mt-0.5 shrink-0" />
          <span className="whitespace-pre-wrap">{visibleError}</span>
          <button
            onClick={() => setError(null)}
            className="ml-auto rounded-md p-1 hover:bg-[var(--color-error)]/10"
            title="Dismiss"
          >
            <X size={12} />
          </button>
        </div>
      )}

      {visibleUnattended.length > 0 && (
        <section className="space-y-2">
          <div className="flex items-center justify-between gap-3">
            <div className="flex items-center gap-2">
              <h3 className="text-sm font-semibold text-[var(--text-primary)]">EKO review queue</h3>
              <span className="text-xs text-[var(--text-tertiary)]">
                {visibleUnattended.length}
              </span>
            </div>
            <button
              onClick={handleCleanup}
              disabled={visibleCleaning || cleanableCount === 0}
              className="btn flex items-center gap-2 disabled:opacity-50"
            >
              {visibleCleaning ? (
                <Loader2 size={14} className="animate-spin" />
              ) : (
                <RotateCcw size={14} />
              )}
              Clean unchanged ({cleanableCount})
            </button>
          </div>

          {visibleUnattended.map((item) => {
            const pendingAction =
              visibleReviewAction?.runId === item.run_id ? visibleReviewAction.action : null;
            return (
              <div
                key={item.run_id}
                className="group flex items-center gap-3 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] p-3"
              >
                <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-[var(--bg-secondary)] text-[var(--text-secondary)]">
                  {item.locked ? <Unlock size={16} /> : <GitBranch size={16} />}
                </div>

                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="truncate text-[13px] font-medium text-[var(--text-primary)]">
                      {item.branch}
                    </span>
                    <span
                      className={`text-[10px] font-medium ${
                        item.has_changes
                          ? 'text-[var(--color-warning)]'
                          : 'text-[var(--color-success)]'
                      }`}
                    >
                      {item.has_changes ? 'changes' : 'unchanged'}
                    </span>
                    {item.active && (
                      <span className="text-[10px] font-medium text-[var(--accent)]">active</span>
                    )}
                    {item.locked && !item.active && (
                      <span className="text-[10px] font-medium text-[var(--color-warning)]">
                        stale lock
                      </span>
                    )}
                    {item.orphan_branch && (
                      <span className="text-[10px] text-[var(--text-tertiary)]">orphan branch</span>
                    )}
                  </div>
                  <div className="mt-0.5 flex min-w-0 items-center gap-2 text-[11px] text-[var(--text-tertiary)]">
                    <FolderOpen size={10} className="shrink-0" />
                    <span className="truncate font-mono">
                      {item.path ?? 'No checkout directory'}
                    </span>
                    <span className="shrink-0">{item.head}</span>
                    {item.ahead_commits > 0 && (
                      <span className="shrink-0">+{item.ahead_commits} commits</span>
                    )}
                  </div>
                </div>

                <div className="flex shrink-0 items-center gap-1">
                  {pendingAction ? (
                    <>
                      <button
                        onClick={() => handleReviewAction(item.run_id, pendingAction)}
                        disabled={visibleReviewing === item.run_id}
                        className={`rounded-md px-2 py-1 text-[11px] font-medium text-white disabled:opacity-50 ${
                          pendingAction === 'merge'
                            ? 'bg-[var(--accent)]'
                            : 'bg-[var(--color-error)]'
                        }`}
                      >
                        {visibleReviewing === item.run_id ? (
                          <Loader2 size={11} className="animate-spin" />
                        ) : (
                          'Confirm'
                        )}
                      </button>
                      <button
                        onClick={() => setReviewAction(null)}
                        className="rounded-md p-1.5 text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
                        title="Cancel"
                      >
                        <X size={13} />
                      </button>
                    </>
                  ) : (
                    <>
                      <button
                        onClick={() => chooseReviewAction(item.run_id, 'merge')}
                        disabled={item.active || !item.has_changes}
                        className="rounded-md p-1.5 text-[var(--text-tertiary)] hover:bg-[var(--accent)]/10 hover:text-[var(--accent)] disabled:opacity-30"
                        title="Merge into current checkout"
                      >
                        <GitMerge size={14} />
                      </button>
                      <button
                        onClick={() => chooseReviewAction(item.run_id, 'discard')}
                        disabled={item.active}
                        className="rounded-md p-1.5 text-[var(--text-tertiary)] hover:bg-[var(--color-error)]/10 hover:text-[var(--color-error)] disabled:opacity-30"
                        title="Discard worktree and branch"
                      >
                        <Trash2 size={14} />
                      </button>
                    </>
                  )}
                </div>
              </div>
            );
          })}
        </section>
      )}

      {showCreateForm && (
        <div className="rounded-lg border border-[var(--border-primary)] bg-[var(--bg-secondary)] p-4">
          <h3 className="mb-3 text-sm font-medium text-[var(--text-primary)]">
            Create New Worktree
          </h3>
          <div className="space-y-3">
            <div>
              <label
                htmlFor="worktree-branch"
                className="mb-1 block text-xs font-medium text-[var(--text-secondary)]"
              >
                Branch Name <span className="text-[var(--color-error)]">*</span>
              </label>
              <input
                id="worktree-branch"
                value={newBranch}
                onChange={(e) => setNewBranch(e.target.value)}
                placeholder="feature/my-branch"
                className="input w-full"
                autoFocus
                onKeyDown={(e) => {
                  if (e.key === 'Enter') void handleCreate();
                  if (e.key === 'Escape') setShowCreateForm(false);
                }}
              />
            </div>
            <div>
              <label
                htmlFor="worktree-base"
                className="mb-1 block text-xs font-medium text-[var(--text-secondary)]"
              >
                Base (optional)
              </label>
              <input
                id="worktree-base"
                value={newBase}
                onChange={(e) => setNewBase(e.target.value)}
                placeholder="main, HEAD, or commit hash"
                className="input w-full"
                onKeyDown={(e) => {
                  if (e.key === 'Enter') void handleCreate();
                  if (e.key === 'Escape') setShowCreateForm(false);
                }}
              />
            </div>
            <div className="flex items-center gap-2 pt-1">
              <button
                onClick={handleCreate}
                disabled={!newBranch.trim() || visibleCreating}
                className="btn btn-primary flex items-center gap-2 disabled:opacity-50"
              >
                {visibleCreating ? (
                  <Loader2 size={14} className="animate-spin" />
                ) : (
                  <Check size={14} />
                )}
                Create
              </button>
              <button
                onClick={() => {
                  setShowCreateForm(false);
                  setNewBranch('');
                  setNewBase('');
                }}
                className="btn"
              >
                Cancel
              </button>
            </div>
          </div>
        </div>
      )}

      {visibleWorktrees.length === 0 ? (
        <div className="rounded-lg border border-dashed border-[var(--border-primary)] p-8 text-center">
          <GitBranch size={32} className="mx-auto mb-3 text-[var(--text-tertiary)]" />
          <p className="text-sm text-[var(--text-secondary)]">No worktrees found</p>
        </div>
      ) : (
        <div className="space-y-2">
          {visibleWorktrees.map((worktree) => {
            const isMain = !worktree.managed;
            const isSubagent = worktree.branch.startsWith('eko-fork-');
            return (
              <div
                key={worktree.path}
                className="group flex items-center gap-3 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] p-3 transition-colors hover:bg-[var(--bg-hover)]"
              >
                <div
                  className={`flex h-9 w-9 shrink-0 items-center justify-center rounded-lg ${
                    isMain
                      ? 'bg-[var(--accent)]/10 text-[var(--accent)]'
                      : 'bg-[var(--bg-secondary)] text-[var(--text-secondary)]'
                  }`}
                >
                  <GitBranch size={16} />
                </div>

                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <span className="truncate text-[13px] font-medium text-[var(--text-primary)]">
                      {worktree.branch || '(no branch)'}
                    </span>
                    {isMain && <span className="text-[10px] text-[var(--accent)]">main</span>}
                    {isSubagent && (
                      <span className="text-[10px] text-[var(--color-success)]">EKO Subagent</span>
                    )}
                  </div>
                  <div className="mt-0.5 flex items-center gap-2 text-[11px] text-[var(--text-tertiary)]">
                    <FolderOpen size={10} />
                    <span className="truncate font-mono">{worktree.path}</span>
                    {worktree.head && <span className="font-mono">{worktree.head}</span>}
                  </div>
                </div>

                {!isMain && (
                  <div className="flex shrink-0 items-center gap-1 opacity-0 transition-opacity group-hover:opacity-100">
                    {confirmDelete === worktree.path ? (
                      <>
                        <button
                          onClick={() => handleRemove(worktree.path)}
                          disabled={visibleRemoving === worktree.path}
                          className="rounded-md bg-[var(--color-error)] px-2 py-1 text-[11px] font-medium text-white disabled:opacity-50"
                        >
                          {visibleRemoving === worktree.path ? (
                            <Loader2 size={11} className="animate-spin" />
                          ) : (
                            'Confirm'
                          )}
                        </button>
                        <button
                          onClick={() => setConfirmDelete(null)}
                          className="rounded-md p-1.5 text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
                          title="Cancel"
                        >
                          <X size={13} />
                        </button>
                      </>
                    ) : (
                      <button
                        onClick={() => setConfirmDelete(worktree.path)}
                        className="rounded-md p-1.5 text-[var(--text-tertiary)] hover:bg-[var(--color-error)]/10 hover:text-[var(--color-error)]"
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
