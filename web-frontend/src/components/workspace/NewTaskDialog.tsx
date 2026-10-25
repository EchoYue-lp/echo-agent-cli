import { useState, useEffect, useCallback } from 'react';
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
  const [error, setError] = useState('');
  const [showPicker, setShowPicker] = useState(false);

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
    const name = newName.trim();
    if (!name) {
      setError('请输入工作空间名称');
      return;
    }
    setCreating(true);
    setError('');
    try {
      const intent = source.kind === 'ssh' ? remoteSource : localProjectSource(source);
      if (!intent) throw new Error('请先完成 SSH 认证并验证远程目录');
      await createAndSwitch(name, newKind, undefined, intent);
      onClose();
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : '创建失败';
      setError(msg);
    } finally {
      setCreating(false);
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
    if (!reconnecting || remoteSource?.kind !== 'ssh') return;
    setCreating(true);
    setError('');
    try {
      await switchTo(reconnecting.id, remoteSource.connection_id);
      onClose();
    } catch (error) {
      setError(error instanceof Error ? error.message : '连接失败');
    } finally {
      setCreating(false);
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
                onClick={() => {
                  setView('list');
                  setError('');
                }}
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
            className="flex items-center justify-end gap-2 border-t px-5 py-3"
            style={{ borderColor: 'var(--border-primary)' }}
          >
            <button
              disabled={creating}
              onClick={() => {
                setView('list');
                setError('');
              }}
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
                  : !newName.trim() || (source.kind === 'ssh' && !remoteSource))
              }
              className="rounded-lg px-5 py-2 text-sm font-medium text-[var(--text-on-accent)] transition-colors disabled:opacity-50"
              style={{ background: 'var(--accent)' }}
            >
              {creating ? '处理中…' : view === 'reconnect' ? '连接并进入' : '创建并进入'}
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
