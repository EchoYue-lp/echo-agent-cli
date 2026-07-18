import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react';
import {
  AlertTriangle,
  Braces,
  Check,
  ExternalLink,
  FileCode2,
  FlaskConical,
  GitBranch,
  Loader2,
  Plus,
  Play,
  RefreshCw,
  Save,
  Square,
  Table2,
  X,
} from 'lucide-react';
import {
  analysisApi,
  type AnalysisDocument,
  type AnalysisLanguage,
  type AnalysisRunStatus,
  type AnalysisSummary,
} from '../../api/endpoints';
import { errorMessage, fileSystem } from '../../lib/tauri-bridge';
import { useToastStore } from '../../stores/toastStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';

type WorkbenchTab = 'code' | 'results' | 'lineage';

interface Draft {
  title: string;
  script: string;
  inputs: string;
  parameters: string;
  randomSeed: string;
}

function draftFromDocument(document: AnalysisDocument): Draft {
  return {
    title: document.manifest.title,
    script: document.script,
    inputs: document.manifest.input_paths.join('\n'),
    parameters: JSON.stringify(document.manifest.parameters ?? {}, null, 2),
    randomSeed:
      document.manifest.random_seed === null || document.manifest.random_seed === undefined
        ? ''
        : String(document.manifest.random_seed),
  };
}

export function formatAnalysisBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

function statusLabel(status?: AnalysisRunStatus | null): string {
  switch (status) {
    case 'succeeded':
      return '成功';
    case 'failed':
      return '失败';
    case 'cancelled':
      return '已取消';
    case 'timed_out':
      return '超时';
    default:
      return '未运行';
  }
}

function statusClass(status?: AnalysisRunStatus | null): string {
  switch (status) {
    case 'succeeded':
      return 'text-[var(--color-success)]';
    case 'failed':
    case 'timed_out':
      return 'text-[var(--color-error)]';
    case 'cancelled':
      return 'text-[var(--color-warning)]';
    default:
      return 'text-[var(--text-tertiary)]';
  }
}

export default function AnalysisPanel() {
  const workspaceId = useWorkspaceStore((state) => state.current?.id ?? null);
  const addToast = useToastStore((state) => state.addToast);
  const [summaries, setSummaries] = useState<AnalysisSummary[]>([]);
  const [document, setDocument] = useState<AnalysisDocument | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [activeTab, setActiveTab] = useState<WorkbenchTab>('code');
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [running, setRunning] = useState(false);
  const [creating, setCreating] = useState(false);
  const [newTitle, setNewTitle] = useState('');
  const [newLanguage, setNewLanguage] = useState<AnalysisLanguage>('python');

  const loadDocument = useCallback(
    async (analysisId: string) => {
      setLoading(true);
      try {
        const loaded = await analysisApi.get(analysisId);
        setDocument(loaded);
        setDraft(draftFromDocument(loaded));
      } catch (error) {
        addToast('error', errorMessage(error));
      } finally {
        setLoading(false);
      }
    },
    [addToast]
  );

  const refreshList = useCallback(
    async (preferredId?: string) => {
      setLoading(true);
      try {
        const list = await analysisApi.list();
        setSummaries(list);
        const selectedId = preferredId ?? list[0]?.analysis_id;
        if (selectedId) {
          const loaded = await analysisApi.get(selectedId);
          setDocument(loaded);
          setDraft(draftFromDocument(loaded));
        } else {
          setDocument(null);
          setDraft(null);
        }
      } catch (error) {
        addToast('error', errorMessage(error));
      } finally {
        setLoading(false);
      }
    },
    [addToast]
  );

  useEffect(() => {
    setDocument(null);
    setDraft(null);
    setSummaries([]);
    void refreshList();
  }, [workspaceId, refreshList]);

  const dirty = useMemo(() => {
    if (!document || !draft) return false;
    const original = draftFromDocument(document);
    return (
      draft.title !== original.title ||
      draft.script !== original.script ||
      draft.inputs !== original.inputs ||
      draft.parameters !== original.parameters ||
      draft.randomSeed !== original.randomSeed
    );
  }, [document, draft]);

  const persistDraft = useCallback(async (): Promise<AnalysisDocument | null> => {
    if (!document || !draft) return null;
    let parameters: unknown;
    try {
      parameters = JSON.parse(draft.parameters || '{}');
    } catch {
      addToast('error', '参数必须是有效 JSON');
      return null;
    }
    const seed = draft.randomSeed.trim() ? Number.parseInt(draft.randomSeed, 10) : null;
    if (seed !== null && (!Number.isSafeInteger(seed) || seed < 0)) {
      addToast('error', '随机种子必须是非负整数');
      return null;
    }

    setSaving(true);
    try {
      const saved = await analysisApi.save(document.manifest.analysis_id, {
        title: draft.title,
        script: draft.script,
        expectedScriptRevision: document.script_revision,
        inputPaths: draft.inputs
          .split('\n')
          .map((value) => value.trim())
          .filter(Boolean),
        parameters,
        randomSeed: seed,
      });
      setDocument(saved);
      setDraft(draftFromDocument(saved));
      setSummaries((current) =>
        current.map((item) =>
          item.analysis_id === saved.manifest.analysis_id
            ? {
                ...item,
                title: saved.manifest.title,
                updated_at: saved.manifest.updated_at,
                stale: saved.stale,
                stale_reasons: saved.stale_reasons,
              }
            : item
        )
      );
      addToast('success', '分析已保存');
      return saved;
    } catch (error) {
      addToast('error', errorMessage(error));
      return null;
    } finally {
      setSaving(false);
    }
  }, [addToast, document, draft]);

  const handleRun = async () => {
    if (!document) return;
    let current = document;
    if (dirty) {
      const saved = await persistDraft();
      if (!saved) return;
      current = saved;
    }
    setRunning(true);
    setActiveTab('results');
    try {
      const completed = await analysisApi.run(current.manifest.analysis_id);
      setDocument(completed);
      setDraft(draftFromDocument(completed));
      await refreshList(completed.manifest.analysis_id);
      const status = completed.last_run?.status;
      addToast(status === 'succeeded' ? 'success' : 'warning', `分析${statusLabel(status)}`);
    } catch (error) {
      addToast('error', errorMessage(error));
    } finally {
      setRunning(false);
    }
  };

  const handleCancel = async () => {
    if (!document) return;
    try {
      const cancelled = await analysisApi.cancel(document.manifest.analysis_id);
      if (!cancelled) addToast('info', '当前分析没有运行中的进程');
    } catch (error) {
      addToast('error', errorMessage(error));
    }
  };

  const handleCreate = async () => {
    const title = newTitle.trim();
    if (!title) return;
    setLoading(true);
    try {
      const created = await analysisApi.create(title, newLanguage);
      setNewTitle('');
      setCreating(false);
      setActiveTab('code');
      await refreshList(created.manifest.analysis_id);
    } catch (error) {
      addToast('error', errorMessage(error));
    } finally {
      setLoading(false);
    }
  };

  const handleOpenArtifact = async (path: string) => {
    try {
      await fileSystem.openArtifact(path);
    } catch (error) {
      addToast('error', errorMessage(error));
    }
  };

  const lastRun = document?.last_run;

  return (
    <div className="flex h-full min-h-0 flex-col bg-[var(--bg-primary)]">
      <div className="flex h-9 shrink-0 items-center gap-1 border-b border-[var(--border-primary)] px-2">
        <button
          type="button"
          onClick={() => setCreating((value) => !value)}
          className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          title="新建分析"
        >
          <Plus size={14} />
        </button>
        <button
          type="button"
          onClick={() => void refreshList(document?.manifest.analysis_id)}
          className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          title="刷新分析"
        >
          <RefreshCw size={13} className={loading ? 'animate-spin' : ''} />
        </button>
        <span className="ml-auto text-[10px] text-[var(--text-tertiary)]">
          {summaries.length} analyses
        </span>
      </div>

      {creating && (
        <div className="flex shrink-0 items-center gap-1 border-b border-[var(--border-primary)] p-2">
          <select
            value={newLanguage}
            onChange={(event) => setNewLanguage(event.target.value as AnalysisLanguage)}
            className="h-7 rounded-md border border-[var(--border-primary)] bg-[var(--bg-secondary)] px-1.5 text-xs text-[var(--text-primary)] outline-none"
            aria-label="分析语言"
          >
            <option value="python">Python</option>
            <option value="r">R</option>
          </select>
          <input
            value={newTitle}
            onChange={(event) => setNewTitle(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter') void handleCreate();
            }}
            className="h-7 min-w-0 flex-1 rounded-md border border-[var(--border-primary)] bg-[var(--bg-secondary)] px-2 text-xs text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]"
            placeholder="分析名称"
            autoFocus
          />
          <button
            type="button"
            onClick={() => void handleCreate()}
            disabled={!newTitle.trim() || loading}
            className="flex h-7 w-7 items-center justify-center rounded-md bg-[var(--accent)] text-white disabled:opacity-40"
            title="创建"
          >
            <Check size={13} />
          </button>
          <button
            type="button"
            onClick={() => setCreating(false)}
            className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)]"
            title="取消"
          >
            <X size={13} />
          </button>
        </div>
      )}

      <div className="h-28 shrink-0 overflow-y-auto border-b border-[var(--border-primary)]">
        {summaries.length === 0 && !loading ? (
          <div className="flex h-full items-center justify-center text-xs text-[var(--text-tertiary)]">
            暂无分析
          </div>
        ) : (
          summaries.map((summary) => (
            <button
              type="button"
              key={summary.analysis_id}
              onClick={() => void loadDocument(summary.analysis_id)}
              className={`flex w-full items-center gap-2 border-b border-[var(--border-secondary)] px-2.5 py-2 text-left transition-colors ${
                document?.manifest.analysis_id === summary.analysis_id
                  ? 'bg-[var(--bg-selected)]'
                  : 'hover:bg-[var(--bg-hover)]'
              }`}
            >
              <FileCode2 size={13} className="shrink-0 text-[var(--accent)]" />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-xs font-medium text-[var(--text-primary)]">
                  {summary.title}
                </span>
                <span className="block truncate text-[10px] text-[var(--text-tertiary)]">
                  {summary.language} · {statusLabel(summary.last_run_status)}
                </span>
              </span>
              {summary.stale && (
                <AlertTriangle size={12} className="shrink-0 text-[var(--color-warning)]" />
              )}
            </button>
          ))
        )}
      </div>

      {!document || !draft ? (
        <div className="flex min-h-0 flex-1 items-center justify-center">
          {loading ? (
            <Loader2 size={18} className="animate-spin text-[var(--text-tertiary)]" />
          ) : (
            <FlaskConical size={22} className="text-[var(--text-tertiary)]" />
          )}
        </div>
      ) : (
        <div className="flex min-h-0 flex-1 flex-col">
          <div className="flex h-10 shrink-0 items-center gap-1 border-b border-[var(--border-primary)] px-2">
            <input
              value={draft.title}
              onChange={(event) => setDraft({ ...draft, title: event.target.value })}
              className="min-w-0 flex-1 bg-transparent text-xs font-medium text-[var(--text-primary)] outline-none"
              aria-label="分析标题"
            />
            <span className="rounded bg-[var(--bg-secondary)] px-1.5 py-0.5 font-mono text-[10px] text-[var(--text-tertiary)]">
              {document.manifest.language}
            </span>
            <button
              type="button"
              onClick={() => void persistDraft()}
              disabled={!dirty || saving || running}
              className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] disabled:opacity-35"
              title="保存"
            >
              {saving ? <Loader2 size={13} className="animate-spin" /> : <Save size={13} />}
            </button>
            {running ? (
              <button
                type="button"
                onClick={() => void handleCancel()}
                className="flex h-7 w-7 items-center justify-center rounded-md bg-[var(--color-error-bg)] text-[var(--color-error)]"
                title="取消运行"
              >
                <Square size={12} />
              </button>
            ) : (
              <button
                type="button"
                onClick={() => void handleRun()}
                className="flex h-7 w-7 items-center justify-center rounded-md bg-[var(--action-run)] text-[var(--text-on-run)]"
                title="运行已保存脚本"
              >
                <Play size={12} />
              </button>
            )}
          </div>

          <div className="flex h-9 shrink-0 items-center border-b border-[var(--border-primary)] px-1">
            <TabButton
              active={activeTab === 'code'}
              icon={<Braces size={12} />}
              label="代码"
              onClick={() => setActiveTab('code')}
            />
            <TabButton
              active={activeTab === 'results'}
              icon={<Table2 size={12} />}
              label="结果"
              onClick={() => setActiveTab('results')}
            />
            <TabButton
              active={activeTab === 'lineage'}
              icon={<GitBranch size={12} />}
              label="Lineage"
              onClick={() => setActiveTab('lineage')}
            />
            <span className="ml-auto px-2 text-[10px] text-[var(--text-tertiary)]">
              {dirty ? '未保存' : document.stale ? 'stale' : 'current'}
            </span>
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto">
            {activeTab === 'code' && (
              <div className="flex min-h-full flex-col">
                <textarea
                  value={draft.script}
                  onChange={(event) => setDraft({ ...draft, script: event.target.value })}
                  spellCheck={false}
                  className="min-h-[280px] flex-1 resize-y border-b border-[var(--border-primary)] bg-[var(--bg-code)] p-3 font-mono text-xs leading-5 text-[var(--text-code)] outline-none"
                  aria-label="分析脚本"
                />
                <div className="grid gap-2 p-2 sm:grid-cols-2">
                  <label className="min-w-0 text-[10px] font-medium text-[var(--text-tertiary)]">
                    输入路径
                    <textarea
                      value={draft.inputs}
                      onChange={(event) => setDraft({ ...draft, inputs: event.target.value })}
                      className="mt-1 h-20 w-full resize-none rounded-md border border-[var(--border-primary)] bg-[var(--bg-secondary)] p-2 font-mono text-[11px] text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]"
                      placeholder="data/raw/input.csv"
                    />
                  </label>
                  <div className="grid min-w-0 grid-rows-[auto_1fr] gap-2">
                    <label className="text-[10px] font-medium text-[var(--text-tertiary)]">
                      随机种子
                      <input
                        type="number"
                        min="0"
                        step="1"
                        value={draft.randomSeed}
                        onChange={(event) => setDraft({ ...draft, randomSeed: event.target.value })}
                        className="mt-1 h-7 w-full rounded-md border border-[var(--border-primary)] bg-[var(--bg-secondary)] px-2 font-mono text-[11px] text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]"
                      />
                    </label>
                    <label className="text-[10px] font-medium text-[var(--text-tertiary)]">
                      参数 JSON
                      <textarea
                        value={draft.parameters}
                        onChange={(event) => setDraft({ ...draft, parameters: event.target.value })}
                        className="mt-1 h-20 w-full resize-none rounded-md border border-[var(--border-primary)] bg-[var(--bg-secondary)] p-2 font-mono text-[11px] text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]"
                      />
                    </label>
                  </div>
                </div>
              </div>
            )}

            {activeTab === 'results' && (
              <div className="divide-y divide-[var(--border-secondary)]">
                <div className="flex items-center gap-3 px-3 py-2 text-xs">
                  {running && <Loader2 size={13} className="animate-spin text-[var(--accent)]" />}
                  <span className={statusClass(lastRun?.status)}>
                    {running ? '运行中' : statusLabel(lastRun?.status)}
                  </span>
                  {lastRun && (
                    <>
                      <span className="text-[var(--text-tertiary)]">{lastRun.duration_ms} ms</span>
                      <span className="font-mono text-[var(--text-tertiary)]">
                        exit {lastRun.exit_code ?? '—'}
                      </span>
                    </>
                  )}
                </div>
                {(lastRun?.output || lastRun?.error) && (
                  <pre className="max-h-64 overflow-auto whitespace-pre-wrap p-3 font-mono text-[11px] leading-5 text-[var(--text-primary)]">
                    {lastRun.output}
                    {lastRun.error ? `\n${lastRun.error}` : ''}
                  </pre>
                )}
                <div>
                  {document.outputs.map((artifact) => (
                    <button
                      type="button"
                      key={artifact.path}
                      onClick={() => void handleOpenArtifact(artifact.absolute_path)}
                      className="flex w-full items-center gap-2 border-b border-[var(--border-secondary)] px-3 py-2 text-left hover:bg-[var(--bg-hover)]"
                    >
                      <ExternalLink size={12} className="shrink-0 text-[var(--accent)]" />
                      <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-[var(--text-primary)]">
                        {artifact.path}
                      </span>
                      <span className="text-[10px] text-[var(--text-tertiary)]">
                        {artifact.kind} · {formatAnalysisBytes(artifact.bytes)}
                      </span>
                    </button>
                  ))}
                  {!running && !lastRun && (
                    <div className="p-6 text-center text-xs text-[var(--text-tertiary)]">
                      未运行
                    </div>
                  )}
                </div>
              </div>
            )}

            {activeTab === 'lineage' && (
              <div className="divide-y divide-[var(--border-secondary)] text-xs">
                {document.stale && (
                  <div className="space-y-1 bg-[var(--color-warning-bg)] px-3 py-2 text-[var(--color-warning)]">
                    {document.stale_reasons.map((reason) => (
                      <div key={reason}>{reason}</div>
                    ))}
                  </div>
                )}
                <LineageRow
                  label="Script"
                  value={lastRun?.script.sha256 ?? document.script_revision}
                  detail={document.manifest.script_path}
                />
                {lastRun?.inputs.map((input) => (
                  <LineageRow
                    key={input.path}
                    label={input.available ? 'Input' : 'Missing'}
                    value={input.sha256 ?? 'unavailable'}
                    detail={input.path}
                  />
                ))}
                {lastRun && (
                  <LineageRow
                    label="Parameters"
                    value={lastRun.parameters_sha256}
                    detail={JSON.stringify(lastRun.parameters)}
                  />
                )}
                {lastRun?.random_seed !== null && lastRun?.random_seed !== undefined && (
                  <LineageRow label="Seed" value={String(lastRun.random_seed)} />
                )}
                {lastRun &&
                  Object.entries(lastRun.environment).map(([name, value]) => (
                    <LineageRow key={name} label={name} value={value} />
                  ))}
                {lastRun?.sandbox_type && (
                  <LineageRow label="Sandbox" value={lastRun.sandbox_type} />
                )}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function TabButton({
  active,
  icon,
  label,
  onClick,
}: {
  active: boolean;
  icon: ReactNode;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`relative flex h-9 items-center gap-1.5 px-2.5 text-[11px] ${
        active
          ? 'text-[var(--text-primary)] after:absolute after:inset-x-2 after:bottom-0 after:h-0.5 after:bg-[var(--accent)]'
          : 'text-[var(--text-tertiary)] hover:text-[var(--text-primary)]'
      }`}
    >
      {icon}
      {label}
    </button>
  );
}

function LineageRow({ label, value, detail }: { label: string; value: string; detail?: string }) {
  return (
    <div className="grid grid-cols-[72px_minmax(0,1fr)] gap-2 px-3 py-2">
      <span className="text-[10px] font-medium uppercase text-[var(--text-tertiary)]">{label}</span>
      <span className="min-w-0">
        {detail && (
          <span className="block truncate text-[11px] text-[var(--text-primary)]">{detail}</span>
        )}
        <span className="block truncate font-mono text-[10px] text-[var(--text-tertiary)]">
          {value}
        </span>
      </span>
    </div>
  );
}
