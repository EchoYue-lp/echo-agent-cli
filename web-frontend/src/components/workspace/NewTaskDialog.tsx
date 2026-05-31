import { useState, useEffect, useCallback } from 'react';
import {
  X, FolderPlus, Code, BarChart3, GraduationCap, MessageSquare,
  ArrowRight, Trash2, FolderOpen, Edit3, RotateCcw,
} from 'lucide-react';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { workspaceApi } from '../../api/endpoints';
import { fileSystem, isTauri } from '../../lib/tauri-bridge';
import DirectoryPicker from './DirectoryPicker';

interface Props {
  isOpen: boolean;
  onClose: () => void;
}

const WORKSPACE_KIND = [
  { value: 'code', label: '代码项目', icon: Code, desc: '代码开发、调试、重构', color: '#6366f1' },
  { value: 'data', label: '数据分析', icon: BarChart3, desc: '数据清洗、分析、可视化', color: '#f59e0b' },
  { value: 'research', label: '学术研究', icon: GraduationCap, desc: '文献检索、论文阅读与写作', color: '#10b981' },
  { value: 'general', label: '通用', icon: MessageSquare, desc: '通用对话与任务', color: '#6b7280' },
];

export default function NewTaskDialog({ isOpen, onClose }: Props) {
  const workspaces = useWorkspaceStore((s) => s.workspaces);
  const current = useWorkspaceStore((s) => s.current);
  const init = useWorkspaceStore((s) => s.init);
  const switchTo = useWorkspaceStore((s) => s.switchTo);
  const createAndSwitch = useWorkspaceStore((s) => s.createAndSwitch);
  const deleteWorkspace = useWorkspaceStore((s) => s.delete);

  const [view, setView] = useState<'list' | 'create'>('list');
  const [newName, setNewName] = useState('');
  const [newKind, setNewKind] = useState('general');
  const [customRoot, setCustomRoot] = useState('');
  const [defaultRoot, setDefaultRoot] = useState('');
  const [useCustomRoot, setUseCustomRoot] = useState(false);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState('');
  const [showPicker, setShowPicker] = useState(false);

  useEffect(() => {
    if (isOpen) {
      init();
      setView('list');
      setNewName('');
      setNewKind('general');
      setCustomRoot('');
      setDefaultRoot('');
      setUseCustomRoot(false);
      setError('');
    }
  }, [isOpen, init]);

  // Fetch default root path when name changes
  useEffect(() => {
    const trimmed = newName.trim();
    if (!trimmed) {
      setDefaultRoot('');
      return;
    }
    let cancelled = false;
    workspaceApi.defaultRoot(trimmed).then((res) => {
      if (!cancelled) setDefaultRoot(res.default_root);
    }).catch(() => {
      if (!cancelled) setDefaultRoot('');
    });
    return () => { cancelled = true; };
  }, [newName]);

  const handleBrowse = useCallback(async () => {
    if (isTauri()) {
      const selected = await fileSystem.selectDirectory('选择工作区目录');
      if (selected) {
        setCustomRoot(selected);
        setUseCustomRoot(true);
      }
    } else {
      setShowPicker(true);
    }
  }, []);

  const handlePickerSelect = useCallback((path: string) => {
    setCustomRoot(path);
    setUseCustomRoot(true);
    setShowPicker(false);
  }, []);

  if (!isOpen) return null;

  const handleCreate = async () => {
    const name = newName.trim();
    if (!name) {
      setError('请输入任务名称');
      return;
    }
    setCreating(true);
    setError('');
    try {
      const root = useCustomRoot && customRoot.trim() ? customRoot.trim() : undefined;
      await createAndSwitch(name, newKind, root);
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
    }
  };

  const handleDelete = async (id: string, e: React.MouseEvent) => {
    e.stopPropagation();
    if (!confirm('确定删除此工作区？所有数据将被清除。')) return;
    try {
      await deleteWorkspace(id);
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : '删除失败';
      setError(msg);
    }
  };

  const kindIcon = (kind: { type: string }) => {
    const k = WORKSPACE_KIND.find((w) => w.value === kind.type);
    if (!k) return <MessageSquare size={16} />;
    const Icon = k.icon;
    return <Icon size={16} style={{ color: k.color }} />;
  };

  return (
    <>
      {/* Backdrop */}
      <div className="fixed inset-0 z-50 bg-black/50" onClick={onClose} />

      {/* Dialog */}
      <div
        className="fixed left-1/2 top-1/2 z-50 flex w-[540px] -translate-x-1/2 -translate-y-1/2 flex-col overflow-hidden rounded-2xl border shadow-2xl"
        style={{ background: 'var(--bg-primary)', borderColor: 'var(--border-primary)', maxHeight: '85vh' }}
      >
        {/* Header */}
        <div className="flex items-center justify-between border-b px-5 py-4" style={{ borderColor: 'var(--border-primary)' }}>
          <h2 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
            {view === 'list' ? '新建任务' : '创建新任务'}
          </h2>
          <div className="flex items-center gap-2">
            {view === 'create' && (
              <button
                onClick={() => { setView('list'); setError(''); }}
                className="text-xs px-2 py-1 rounded hover:opacity-80"
                style={{ color: 'var(--text-secondary)' }}
              >
                ← 返回
              </button>
            )}
            <button
              onClick={onClose}
              className="flex h-7 w-7 items-center justify-center rounded-lg transition-colors hover:opacity-80"
              style={{ color: 'var(--text-tertiary)' }}
            >
              <X size={15} />
            </button>
          </div>
        </div>

        {/* Error */}
        {error && (
          <div className="mx-5 mt-3 rounded-lg px-3 py-2 text-xs" style={{ background: '#fee2e2', color: '#dc2626' }}>
            {error}
          </div>
        )}

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-5">
          {view === 'list' ? (
            <div className="space-y-4">
              {/* Current workspace */}
              {current && (
                <div className="rounded-lg border p-3" style={{ borderColor: 'var(--accent)', background: 'var(--bg-sidebar-active, rgba(99,102,241,0.05))' }}>
                  <div className="text-[11px] font-medium uppercase tracking-wider mb-1" style={{ color: 'var(--accent)' }}>
                    当前工作区
                  </div>
                  <div className="flex items-center gap-2">
                    {kindIcon(current.kind)}
                    <span className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>{current.name}</span>
                    <span className="text-[11px] px-1.5 py-0.5 rounded" style={{ background: 'var(--bg-secondary)', color: 'var(--text-tertiary)' }}>
                      {current.kind.type}
                    </span>
                  </div>
                  <div className="text-[11px] mt-1 truncate" style={{ color: 'var(--text-tertiary)' }}>
                    {current.root}
                  </div>
                </div>
              )}

              {/* New workspace button */}
              <button
                onClick={() => setView('create')}
                className="flex w-full items-center gap-3 rounded-xl border-2 border-dashed px-4 py-3.5 text-left transition-all hover:border-solid"
                style={{ borderColor: 'var(--border-primary)', color: 'var(--text-primary)' }}
                onMouseEnter={(e) => { e.currentTarget.style.borderColor = 'var(--accent)'; }}
                onMouseLeave={(e) => { e.currentTarget.style.borderColor = 'var(--border-primary)'; }}
              >
                <FolderPlus size={20} style={{ color: 'var(--accent)' }} />
                <div>
                  <div className="text-sm font-medium">创建新任务</div>
                  <div className="text-[11px]" style={{ color: 'var(--text-tertiary)' }}>新建工作区，隔离所有数据</div>
                </div>
                <ArrowRight size={16} className="ml-auto" style={{ color: 'var(--text-tertiary)' }} />
              </button>

              {/* Existing workspaces */}
              {workspaces.length > 0 && (
                <div>
                  <div className="text-[11px] font-semibold uppercase tracking-wider mb-2" style={{ color: 'var(--text-tertiary)' }}>
                    已有工作区
                  </div>
                  <div className="space-y-1">
                    {workspaces.map((ws) => (
                      <button
                        key={ws.id}
                        onClick={() => handleSwitch(ws.id)}
                        className={`flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left transition-colors
                          ${current?.id === ws.id ? 'opacity-60 cursor-default' : 'hover:bg-[var(--bg-hover)]'}`}
                        disabled={current?.id === ws.id}
                      >
                        {kindIcon(ws.kind)}
                        <div className="min-w-0 flex-1">
                          <div className="text-sm truncate" style={{ color: 'var(--text-primary)' }}>{ws.name}</div>
                          <div className="text-[11px] truncate" style={{ color: 'var(--text-tertiary)' }}>
                            {ws.kind.type} · {ws.root}
                          </div>
                        </div>
                        {current?.id !== ws.id && (
                          <ArrowRight size={14} style={{ color: 'var(--text-tertiary)' }} />
                        )}
                        <button
                          onClick={(e) => handleDelete(ws.id, e)}
                          className="ml-1 rounded p-1 transition-colors hover:text-red-500"
                          style={{ color: 'var(--text-tertiary)' }}
                          title="删除"
                        >
                          <Trash2 size={13} />
                        </button>
                      </button>
                    ))}
                  </div>
                </div>
              )}
            </div>
          ) : (
            <div className="space-y-4">
              {/* Name input */}
              <div>
                <label className="block text-xs font-medium mb-1.5" style={{ color: 'var(--text-secondary)' }}>
                  任务名称
                </label>
                <input
                  type="text"
                  value={newName}
                  onChange={(e) => setNewName(e.target.value)}
                  onKeyDown={(e) => { if (e.key === 'Enter' && !useCustomRoot) handleCreate(); }}
                  placeholder="例：用户行为分析、NLP论文综述..."
                  className="w-full rounded-lg border px-3 py-2.5 text-sm outline-none transition-colors focus:border-[var(--accent)]"
                  style={{ background: 'var(--bg-input)', borderColor: 'var(--border-primary)', color: 'var(--text-primary)' }}
                  autoFocus
                />
              </div>

              {/* Kind selector */}
              <div>
                <label className="block text-xs font-medium mb-1.5" style={{ color: 'var(--text-secondary)' }}>
                  任务类型
                </label>
                <div className="grid grid-cols-2 gap-2">
                  {WORKSPACE_KIND.map((k) => {
                    const Icon = k.icon;
                    const selected = newKind === k.value;
                    return (
                      <button
                        key={k.value}
                        onClick={() => setNewKind(k.value)}
                        className={`flex items-center gap-2.5 rounded-lg border p-3 text-left transition-all ${
                          selected ? 'border-2' : ''
                        }`}
                        style={{
                          borderColor: selected ? k.color : 'var(--border-primary)',
                          background: selected ? `${k.color}10` : 'transparent',
                        }}
                      >
                        <Icon size={18} style={{ color: k.color }} />
                        <div>
                          <div className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>{k.label}</div>
                          <div className="text-[11px]" style={{ color: 'var(--text-tertiary)' }}>{k.desc}</div>
                        </div>
                      </button>
                    );
                  })}
                </div>
              </div>

              {/* Folder path */}
              <div>
                <label className="block text-xs font-medium mb-2" style={{ color: 'var(--text-secondary)' }}>
                  工作目录
                </label>

                {useCustomRoot ? (
                  <div className="space-y-2">
                    <div className="flex gap-2">
                      <input
                        type="text"
                        value={customRoot}
                        onChange={(e) => setCustomRoot(e.target.value)}
                        placeholder="输入目录路径，或点击浏览..."
                        className="flex-1 rounded-lg border px-3 py-2.5 text-sm outline-none transition-colors focus:border-[var(--accent)]"
                        style={{ background: 'var(--bg-input)', borderColor: 'var(--border-primary)', color: 'var(--text-primary)' }}
                        autoFocus
                      />
                      <button
                        onClick={handleBrowse}
                        className="flex items-center gap-1.5 rounded-lg border px-4 py-2.5 text-sm font-medium transition-colors hover:bg-[var(--bg-hover)]"
                        style={{ borderColor: 'var(--accent)', color: 'var(--accent)' }}
                      >
                        <FolderOpen size={14} />
                        浏览
                      </button>
                    </div>
                    <div className="text-[11px]" style={{ color: 'var(--text-tertiary)' }}>
                      将在该目录下创建工作区子目录（sessions/、memory/ 等）
                    </div>
                    <button
                      onClick={() => {
                        setUseCustomRoot(false);
                        setCustomRoot('');
                      }}
                      className="flex items-center gap-1 text-[11px] transition-colors hover:opacity-80"
                      style={{ color: 'var(--text-tertiary)' }}
                    >
                      <RotateCcw size={11} /> 恢复默认路径
                    </button>
                  </div>
                ) : (
                  <div className="space-y-2">
                    <div
                      className="rounded-lg border px-3 py-2.5 text-sm"
                      style={{ background: 'var(--bg-secondary)', borderColor: 'var(--border-primary)', color: 'var(--text-tertiary)' }}
                    >
                      <span className="flex items-center gap-1.5">
                        <FolderOpen size={14} />
                        {defaultRoot || '输入任务名称后显示默认路径'}
                      </span>
                    </div>
                    <button
                      onClick={() => setUseCustomRoot(true)}
                      className="flex items-center gap-1.5 rounded-lg border px-3 py-2 text-xs font-medium transition-colors hover:bg-[var(--bg-hover)]"
                      style={{ borderColor: 'var(--accent)', color: 'var(--accent)' }}
                    >
                      <Edit3 size={12} /> 选择其他文件夹
                    </button>
                  </div>
                )}
              </div>
            </div>
          )}
        </div>

        {/* Footer (create view only) */}
        {view === 'create' && (
          <div className="flex items-center justify-end gap-2 border-t px-5 py-3" style={{ borderColor: 'var(--border-primary)' }}>
            <button
              onClick={() => { setView('list'); setError(''); }}
              className="rounded-lg px-4 py-2 text-sm transition-colors hover:opacity-80"
              style={{ color: 'var(--text-secondary)' }}
            >
              取消
            </button>
            <button
              onClick={handleCreate}
              disabled={creating || !newName.trim()}
              className="rounded-lg px-5 py-2 text-sm font-medium text-white transition-colors disabled:opacity-50"
              style={{ background: 'var(--accent)' }}
            >
              {creating ? '创建中...' : '创建并进入'}
            </button>
          </div>
        )}
      </div>

      {/* Directory Picker (web mode) */}
      <DirectoryPicker
        isOpen={showPicker}
        onClose={() => setShowPicker(false)}
        onSelect={handlePickerSelect}
      />
    </>
  );
}
