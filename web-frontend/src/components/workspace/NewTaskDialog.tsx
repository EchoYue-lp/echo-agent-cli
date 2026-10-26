import { useState, useEffect, useCallback, useRef } from 'react';
import { X, FolderPlus, MoreHorizontal, ArrowRight, Trash2 } from 'lucide-react';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { workspaceApi, type ProjectSource, type Workspace } from '../../api/endpoints';
import { fileSystem, isTauri } from '../../lib/tauri-bridge';
import { WORKSPACE_KINDS as WORKSPACE_KIND } from '../../lib/workspaceKinds';
import { Modal } from '../common/Modal';
import DirectoryPicker from './DirectoryPicker';
import {
  ProjectSourceFields,
  emptyProjectSource,
  localProjectSource,
  type ProjectSourceDraft,
} from './ProjectSourceFields';
import { workspaceLocationLabel } from '../../lib/workspacePresentation';

interface Props {
  isOpen: boolean;
  onClose: () => void;
  initialSource?: ProjectSourceDraft['kind'];
}

function observeDirectory(original: Promise<{ path: string }>): Promise<{ path: string }> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  return Promise.race([
    original,
    new Promise<never>((_, reject) => {
      timer = setTimeout(
        () =>
          reject(
            new Error(
              '远程目录验证尚未确认，未发起创建；可稍后再次点击重试观察，或取消。原验证仍由连接持有。'
            )
          ),
        30_000
      );
    }),
  ]).finally(() => clearTimeout(timer));
}

export default function NewTaskDialog({ isOpen, onClose, initialSource }: Props) {
  const workspaces = useWorkspaceStore((s) => s.workspaces);
  const current = useWorkspaceStore((s) => s.current);
  const init = useWorkspaceStore((s) => s.init);
  const switchTo = useWorkspaceStore((s) => s.switchTo);
  const createAndSwitch = useWorkspaceStore((s) => s.createAndSwitch);
  const deleteWorkspace = useWorkspaceStore((s) => s.delete);

  const [view, setView] = useState<'list' | 'create' | 'reconnect'>('list');
  const [newName, setNewName] = useState('');
  const [newKind, setNewKind] = useState('code');
  const [source, setSource] = useState(emptyProjectSource);
  const [remoteSource, setRemoteSource] = useState<ProjectSource>();
  const [reconnecting, setReconnecting] = useState<Workspace>();
  const [defaultRoot, setDefaultRoot] = useState('');
  const [creating, setCreating] = useState(false);
  const [creationStage, setCreationStage] = useState('处理中…');
  const [error, setError] = useState('');
  const [showPicker, setShowPicker] = useState(false);
  const live = useRef(false);
  const opened = useRef(isOpen);
  opened.current = isOpen;
  const operation = useRef<symbol | null>(null);
  const verification = useRef<{
    key: string;
    owner: string;
    settled: boolean;
    promise: Promise<{ path: string }>;
  } | null>(null);

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
      operation.current = null;
    };
  }, []);
  useEffect(() => {
    operation.current = null;
    setCreating(false);
  }, [isOpen, initialSource]);
  const ownsOperation = (token: symbol) =>
    live.current && opened.current && operation.current === token;
  const verifyRemote = async (intent: Extract<ProjectSource, { kind: 'ssh' }>) => {
    const target = {
      kind: 'ssh' as const,
      host: intent.host,
      user: intent.user,
      port: intent.port,
    };
    const key = JSON.stringify([intent.connection_id, target, intent.path.trim()]);
    let pending = verification.current;
    if (
      pending &&
      !pending.settled &&
      pending.owner === intent.connection_id &&
      pending.key !== key
    )
      throw new Error('原目录验证仍未确认，请等待原操作结算或断开连接；未发起创建。');
    if (!pending || pending.key !== key || pending.settled) {
      const record = {
        key,
        owner: intent.connection_id,
        settled: false,
        promise: workspaceApi.verifySsh(intent.connection_id, target, intent.path.trim()),
      };
      verification.current = record;
      void record.promise.then(
        () => {
          record.settled = true;
        },
        () => {
          record.settled = true;
        }
      );
      pending = record;
    }
    return observeDirectory(pending.promise);
  };

  useEffect(() => {
    if (isOpen) {
      init();
      setView(initialSource ? 'create' : 'list');
      setNewName('');
      setNewKind('code');
      setSource({ ...emptyProjectSource(), kind: initialSource ?? 'existing_local' });
      setRemoteSource(undefined);
      setReconnecting(undefined);
      setDefaultRoot('');
      setError('');
    }
  }, [isOpen, init, initialSource]);

  // Fetch default root path when name changes
  useEffect(() => {
    const trimmed = newName.trim();
    if (!trimmed) {
      setDefaultRoot('');
      return;
    }
    let cancelled = false;
    workspaceApi
      .defaultRoot(trimmed)
      .then((res) => {
        if (!cancelled) setDefaultRoot(res.default_root);
      })
      .catch(() => {
        if (!cancelled) setDefaultRoot('');
      });
    return () => {
      cancelled = true;
    };
  }, [newName]);

  const handleBrowse = useCallback(async () => {
    if (isTauri()) {
      try {
        const selected = await fileSystem.selectDirectory('选择工作空间目录');
        if (selected) {
          setSource((previous) => ({ ...previous, localPath: selected }));
        }
      } catch (e) {
        console.error('Browse directory failed:', e);
        setError(`浏览目录失败: ${e instanceof Error ? e.message : String(e)}`);
      }
    } else {
      setShowPicker(true);
    }
  }, []);

  const handlePickerSelect = useCallback((path: string) => {
    setSource((previous) => ({ ...previous, localPath: path }));
    setShowPicker(false);
  }, []);

  if (!isOpen) return null;

  const handleCreate = async () => {
    if (operation.current) return;
    const name = newName.trim();
    if (!name) {
      setError('请输入工作空间名称');
      return;
    }
    setCreating(true);
    setError('');
    const token = Symbol('create-workspace');
    operation.current = token;
    try {
      let intent = source.kind === 'ssh' ? remoteSource : localProjectSource(source);
      if (!intent) throw new Error('请先测试 SSH 连接');
      if (intent.kind === 'ssh') {
        if (!intent.path.trim()) throw new Error('请输入远程项目文件夹，连接测试不要求目录');
        setCreationStage('正在验证远程目录…');
        const verified = await verifyRemote(intent);
        if (!ownsOperation(token)) return;
        intent = { ...intent, path: verified.path };
      }
      setCreationStage('正在创建工作空间…');
      await createAndSwitch(name, newKind, undefined, intent);
      if (!ownsOperation(token)) return;
      onClose();
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : '创建失败';
      if (ownsOperation(token)) setError(msg);
    } finally {
      if (ownsOperation(token)) {
        operation.current = null;
        setCreating(false);
      }
    }
  };

  const handleSwitch = async (id: string) => {
    try {
      await switchTo(id);
      onClose();
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : '切换失败';
      setError(msg);
      const workspace = workspaces.find((workspace) => workspace.id === id);
      const location = workspace?.metadata.project_location;
      if (workspace && location?.kind === 'ssh') {
        setReconnecting(workspace);
        setRemoteSource(undefined);
        setSource({
          ...emptyProjectSource(),
          kind: 'ssh',
          host: location.host,
          user: location.user ?? '',
          port: location.port?.toString() ?? '',
          remotePath: location.path,
        });
        setView('reconnect');
      }
    }
  };

  const handleReconnect = async () => {
    if (!reconnecting || remoteSource?.kind !== 'ssh' || operation.current) return;
    const token = Symbol('reconnect-workspace');
    operation.current = token;
    setCreating(true);
    setError('');
    try {
      setCreationStage('正在验证原远程目录…');
      await verifyRemote(remoteSource);
      if (!ownsOperation(token)) return;
      setCreationStage('正在连接工作空间…');
      await switchTo(reconnecting.id, remoteSource.connection_id);
      if (!ownsOperation(token)) return;
      onClose();
    } catch (error) {
      if (ownsOperation(token)) setError(error instanceof Error ? error.message : '连接失败');
    } finally {
      if (ownsOperation(token)) {
        operation.current = null;
        setCreating(false);
      }
    }
  };

  const handleDelete = async (id: string, e: React.MouseEvent) => {
    e.stopPropagation();
    if (!confirm('确定移除此项目及其本机会话数据？项目代码文件不会被删除。')) return;
    try {
      await deleteWorkspace(id);
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : '删除失败';
      setError(msg);
    }
  };

  const returnToList = () => {
    operation.current = null;
    setRemoteSource(undefined);
    setView('list');
    setError('');
  };

  const kindIcon = (kind: { type: string }) => {
    const normalized = kind.type === 'data' ? 'data_analysis' : kind.type;
    const k = WORKSPACE_KIND.find((w) => w.value === normalized);
    if (!k) return <MoreHorizontal size={16} />;
    const Icon = k.icon;
    return <Icon size={16} style={{ color: k.color }} />;
  };

  return (
    <>
      <Modal
        onClose={() => {
          if (!creating) onClose();
        }}
        ariaLabel="工作空间"
        active={!showPicker}
        className="flex max-h-[85vh] w-[540px] max-w-[calc(100vw-2rem)] flex-col overflow-hidden rounded-xl border border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-[var(--shadow-xl)]"
      >
        {/* Header */}
        <div
          className="flex items-center justify-between border-b px-5 py-4"
          style={{ borderColor: 'var(--border-primary)' }}
        >
          <h2 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
            {view === 'list'
              ? '工作空间'
              : view === 'reconnect'
                ? '连接已有远程项目'
                : '创建工作空间'}
          </h2>
          <div className="flex items-center gap-2">
            {view !== 'list' && (
              <button
                disabled={creating}
                onClick={returnToList}
                className="text-xs px-2 py-1 rounded-md hover:opacity-80"
                style={{ color: 'var(--text-secondary)' }}
              >
                ← 返回
              </button>
            )}
            <button
              type="button"
              aria-label="关闭工作空间"
              onClick={onClose}
              disabled={creating}
              className="flex h-7 w-7 items-center justify-center rounded-lg transition-colors hover:opacity-80"
              style={{ color: 'var(--text-tertiary)' }}
            >
              <X size={15} />
            </button>
          </div>
        </div>

        {/* Error */}
        {error && (
          <div
            className="mx-5 mt-3 rounded-lg px-3 py-2 text-xs"
            style={{ background: 'var(--color-error-bg)', color: 'var(--color-error-text)' }}
          >
            {error}
          </div>
        )}

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-5">
          {view === 'list' ? (
            <div className="space-y-4">
              {/* Current workspace */}
              {current && (
                <div
                  className="rounded-lg border p-3"
                  style={{
                    borderColor: 'var(--accent)',
                    background: 'var(--bg-sidebar-active)',
                  }}
                >
                  <div
                    className="text-[11px] font-medium uppercase tracking-wider mb-1"
                    style={{ color: 'var(--accent)' }}
                  >
                    当前工作空间
                  </div>
                  <div className="flex items-center gap-2">
                    {kindIcon(current.kind)}
                    <span className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>
                      {current.name}
                    </span>
                    <span
                      className="text-[11px] px-1.5 py-0.5 rounded-md"
                      style={{ background: 'var(--bg-secondary)', color: 'var(--text-tertiary)' }}
                    >
                      {current.kind.type}
                    </span>
                  </div>
                  <div
                    className="text-[11px] mt-1 truncate"
                    style={{ color: 'var(--text-tertiary)' }}
                  >
                    {workspaceLocationLabel(current)}
                  </div>
                </div>
              )}

              {/* New workspace button */}
              <button
                onClick={() => setView('create')}
                className="flex w-full items-center gap-3 rounded-xl border-2 border-dashed px-4 py-3.5 text-left transition-all hover:border-solid"
                style={{ borderColor: 'var(--border-primary)', color: 'var(--text-primary)' }}
                onMouseEnter={(e) => {
                  e.currentTarget.style.borderColor = 'var(--accent)';
                }}
                onMouseLeave={(e) => {
                  e.currentTarget.style.borderColor = 'var(--border-primary)';
                }}
              >
                <FolderPlus size={20} style={{ color: 'var(--accent)' }} />
                <div>
                  <div className="text-sm font-medium">创建工作空间</div>
                  <div className="text-[11px]" style={{ color: 'var(--text-tertiary)' }}>
                    选择已有文件夹、新建文件夹或远程项目
                  </div>
                </div>
                <ArrowRight
                  size={16}
                  className="ml-auto"
                  style={{ color: 'var(--text-tertiary)' }}
                />
              </button>

              {/* Existing workspaces */}
              {workspaces.length > 0 && (
                <div>
                  <div
                    className="text-[11px] font-semibold uppercase tracking-wider mb-2"
                    style={{ color: 'var(--text-tertiary)' }}
                  >
                    已有工作空间
                  </div>
                  <div className="space-y-1">
                    {workspaces.map((ws) => (
                      <div
                        key={ws.id}
                        className={`flex w-full items-center rounded-lg transition-colors ${
                          current?.id === ws.id
                            ? 'cursor-default opacity-60'
                            : 'hover:bg-[var(--bg-hover)]'
                        }`}
                      >
                        <button
                          type="button"
                          onClick={() => handleSwitch(ws.id)}
                          className="flex min-w-0 flex-1 items-center gap-3 px-3 py-2.5 text-left disabled:cursor-default"
                          disabled={current?.id === ws.id}
                        >
                          {kindIcon(ws.kind)}
                          <div className="min-w-0 flex-1">
                            <div
                              className="truncate text-sm"
                              style={{ color: 'var(--text-primary)' }}
                            >
                              {ws.name}
                            </div>
                            <div
                              className="truncate text-[11px]"
                              style={{ color: 'var(--text-tertiary)' }}
                            >
                              {workspaceLocationLabel(ws)}
                            </div>
                          </div>
                          {current?.id !== ws.id && (
                            <ArrowRight size={14} style={{ color: 'var(--text-tertiary)' }} />
                          )}
                        </button>
                        <button
                          type="button"
                          aria-label={`删除工作空间 ${ws.name}`}
                          onClick={(e) => handleDelete(ws.id, e)}
                          className="mr-2 rounded-md p-1 transition-colors hover:text-red-500"
                          style={{ color: 'var(--text-tertiary)' }}
                          title="删除"
                        >
                          <Trash2 size={13} />
                        </button>
                      </div>
                    ))}
                  </div>
                </div>
              )}
            </div>
          ) : view === 'reconnect' ? (
            <div className="space-y-4">
              <p className="text-sm text-[var(--text-primary)]">{reconnecting?.name}</p>
              <p className="text-xs text-[var(--text-secondary)]">
                项目记录已保留。重新认证并验证同一远程位置后进入，不会自动改变项目绑定。
              </p>
              <ProjectSourceFields
                draft={source}
                onChange={setSource}
                onBrowse={handleBrowse}
                onRemoteSource={setRemoteSource}
                busy={creating}
                fixedLocation
              />
            </div>
          ) : (
            <div className="space-y-4">
              {/* Name input */}
              <div>
                <label
                  htmlFor="workspace-name"
                  className="block text-xs font-medium mb-1.5"
                  style={{ color: 'var(--text-secondary)' }}
                >
                  工作空间名称
                </label>
                <input
                  id="workspace-name"
                  disabled={creating}
                  type="text"
                  value={newName}
                  onChange={(e) => setNewName(e.target.value)}
                  onKeyDown={(e) => {
                    // Candidate confirmation must not create or switch projects.
                    // WebKit may clear isComposing before its final Enter.
                    if (e.nativeEvent.isComposing || e.nativeEvent.keyCode === 229) return;
                    if (e.key === 'Enter' && !creating) void handleCreate();
                  }}
                  placeholder="例：用户行为分析、NLP论文综述..."
                  className="w-full rounded-lg border px-3 py-2.5 text-sm outline-none transition-colors focus:border-[var(--accent)]"
                  style={{
                    background: 'var(--bg-input)',
                    borderColor: 'var(--border-primary)',
                    color: 'var(--text-primary)',
                  }}
                  autoFocus
                />
              </div>

              <div>
                <label
                  htmlFor="workspace-purpose"
                  className="mb-1.5 block text-xs font-medium text-[var(--text-secondary)]"
                >
                  用途
                </label>
                <select
                  id="workspace-purpose"
                  value={newKind}
                  disabled={creating}
                  onChange={(event) => setNewKind(event.target.value)}
                  className="w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 py-2.5 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--accent)]"
                >
                  {WORKSPACE_KIND.map((kind) => (
                    <option key={kind.value} value={kind.value}>
                      {kind.label}
                    </option>
                  ))}
                </select>
              </div>

              <ProjectSourceFields
                draft={source}
                onChange={setSource}
                onBrowse={handleBrowse}
                onRemoteSource={setRemoteSource}
                busy={creating}
              />
              <details className="text-[11px] text-[var(--text-tertiary)]">
                <summary className="cursor-pointer">本机数据保存位置</summary>
                <p className="mt-2 break-all">{defaultRoot || '输入名称后显示'}</p>
              </details>
            </div>
          )}
        </div>

        {/* Footer (create view only) */}
        {view !== 'list' && (
          <div
            className="flex flex-wrap items-center justify-end gap-2 border-t px-5 py-3"
            style={{ borderColor: 'var(--border-primary)' }}
          >
            {source.kind === 'ssh' && !creating && (
              <p className="mr-auto w-full text-xs text-[var(--text-tertiary)]" role="status">
                {!remoteSource
                  ? '请先测试 SSH 连接，登录成功后才可创建。'
                  : !source.remotePath.trim()
                    ? '连接已通过，请填写远程项目文件夹。'
                    : '登录已通过；点击后验证远程目录并进入。'}
              </p>
            )}
            <button
              disabled={creating}
              onClick={returnToList}
              className="rounded-lg px-4 py-2 text-sm transition-colors hover:opacity-80"
              style={{ color: 'var(--text-secondary)' }}
            >
              取消
            </button>
            <button
              onClick={view === 'reconnect' ? handleReconnect : handleCreate}
              disabled={
                creating ||
                (view === 'reconnect'
                  ? !remoteSource
                  : !newName.trim() ||
                    (source.kind === 'ssh' && (!remoteSource || !source.remotePath.trim())))
              }
              className="rounded-lg px-5 py-2 text-sm font-medium text-[var(--text-on-accent)] transition-colors disabled:opacity-50"
              style={{ background: 'var(--accent)' }}
            >
              {creating ? creationStage : view === 'reconnect' ? '连接并进入' : '创建并进入'}
            </button>
          </div>
        )}
      </Modal>

      {/* Directory Picker (web mode) */}
      <DirectoryPicker
        isOpen={showPicker}
        onClose={() => setShowPicker(false)}
        onSelect={handlePickerSelect}
      />
    </>
  );
}
