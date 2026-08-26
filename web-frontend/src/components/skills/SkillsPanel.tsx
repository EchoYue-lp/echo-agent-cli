import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { extensionRequestScope, skillsApi } from '../../api/endpoints';
import type { TauriSkillInfo } from '../../types/api';
import { CATEGORY_LABELS } from '../../types/api';
import type {
  ExtensionCommandReceipt,
  ExtensionRequestScope,
  SkillCommandReceipt,
  SkillSyncReceipt,
  SkillUpdateProjection,
} from '../../generated';
import {
  BookOpen,
  ChevronDown,
  ChevronRight,
  FolderOpen,
  Loader2,
  Power,
  Search,
  Star,
  AlertTriangle,
  Download,
  RefreshCw,
  Trash2,
} from 'lucide-react';
import { useToastStore } from '../../stores/toastStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { fileSystem, isTauri } from '../../lib/tauri-bridge';

/** Category sort order for consistent display. */
const CATEGORY_ORDER = [
  'methodology',
  'development',
  'document',
  'design',
  'research',
  'automation',
];

function sameScope(left: ExtensionRequestScope, right: ExtensionRequestScope): boolean {
  return (
    left.workspace_id === right.workspace_id &&
    left.workspace_generation === right.workspace_generation &&
    left.sender_id === right.sender_id &&
    left.sender_incarnation === right.sender_incarnation
  );
}

export function skillCommandReceipt(receipt: ExtensionCommandReceipt): SkillCommandReceipt {
  if (receipt.extension !== 'skills' || receipt.receipt === null) {
    throw new Error(receipt.meta.error ?? 'Skill command returned no typed receipt');
  }
  if (receipt.meta.status === 'failed') {
    throw new Error(receipt.meta.error ?? 'Skill command failed');
  }
  return receipt.receipt;
}

export function SkillsPanel() {
  const workspace = useWorkspaceStore((state) => state.current);
  const requestScope = useMemo(() => extensionRequestScope(workspace), [workspace]);
  const [skills, setSkills] = useState<TauriSkillInfo[]>([]);
  const [skillsOmitted, setSkillsOmitted] = useState(0);
  const [dir, setDir] = useState('');
  const [loading, setLoading] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [query, setQuery] = useState('');
  const [busySkill, setBusySkill] = useState<string | null>(null);
  const [checkingUpdates, setCheckingUpdates] = useState(false);
  const [syncingSkills, setSyncingSkills] = useState(false);
  const [updateStatuses, setUpdateStatuses] = useState<Record<string, SkillUpdateProjection>>({});
  const [updatesOmitted, setUpdatesOmitted] = useState(0);
  const [collapsedCategories, setCollapsedCategories] = useState<Set<string>>(new Set());
  const fileInputRef = useRef<HTMLInputElement>(null);
  const requestSequence = useRef(0);
  const addToast = useToastStore((s) => s.addToast);
  const loadingAny = loading || uploading || checkingUpdates || syncingSkills;

  const filteredSkills = skills;

  /** Group filtered skills by category, maintaining display order. */
  const groupedSkills = useMemo(() => {
    const groups: Record<string, TauriSkillInfo[]> = {};
    for (const sk of filteredSkills) {
      const cat = sk.category || 'other';
      if (!groups[cat]) groups[cat] = [];
      groups[cat].push(sk);
    }
    // Sort categories
    const sorted: [string, TauriSkillInfo[]][] = [];
    for (const cat of CATEGORY_ORDER) {
      if (groups[cat]) sorted.push([cat, groups[cat]]);
    }
    for (const cat of Object.keys(groups).sort()) {
      if (!CATEGORY_ORDER.includes(cat)) sorted.push([cat, groups[cat]]);
    }
    return sorted;
  }, [filteredSkills]);

  const toggleCategory = (cat: string) => {
    setCollapsedCategories((prev) => {
      const next = new Set(prev);
      if (next.has(cat)) next.delete(cat);
      else next.add(cat);
      return next;
    });
  };

  const assertActiveScope = useCallback((expected: ExtensionRequestScope) => {
    const current = extensionRequestScope(useWorkspaceStore.getState().current);
    if (!sameScope(current, expected)) {
      throw new Error('Workspace focus changed before the Skill command settled');
    }
  }, []);

  const refreshSkills = useCallback(
    async (search = query, token?: { active: boolean }) => {
      const sequence = ++requestSequence.current;
      const expectedScope = requestScope;
      const normalized = search.trim();
      const receipt = normalized
        ? await skillsApi.search(expectedScope, normalized)
        : await skillsApi.list(expectedScope);
      assertActiveScope(expectedScope);
      if (token?.active === false || sequence !== requestSequence.current) return [];
      const command = skillCommandReceipt(receipt);
      const bounded =
        command.action === 'listed' || command.action === 'searched' ? command.skills : null;
      if (bounded === null) {
        throw new Error(`Unexpected Skill receipt '${command.action}' while listing skills`);
      }
      setSkills(bounded.items);
      setSkillsOmitted(bounded.omitted);
      return bounded.items;
    },
    [assertActiveScope, query, requestScope]
  );

  useEffect(() => {
    const sequence = ++requestSequence.current;
    const token = { active: true };
    const timeout = window.setTimeout(
      () => {
        if (sequence !== requestSequence.current) return;
        setLoading(true);
        void refreshSkills(query, token)
          .catch((error) => addToast('error', `加载技能失败: ${String(error)}`))
          .finally(() => setLoading(false));
      },
      query.trim() ? 180 : 0
    );
    return () => {
      token.active = false;
      window.clearTimeout(timeout);
    };
  }, [addToast, query, refreshSkills, requestScope]);

  const showSettlement = (action: string, receipt: SkillSyncReceipt, settledMessage: string) => {
    if (receipt.status === 'settled') {
      addToast('success', settledMessage);
      return;
    }

    if (receipt.status === 'committed') {
      addToast('info', `${action}已写入持久配置，运行时仍在结算`, 8000);
      return;
    }

    const repair = receipt.repair_debt ? '，已记录自动修复任务' : '';
    addToast('warning', `${action}已写入持久配置，但部分运行时目标未完成${repair}`, 8000);
  };

  const loadPath = async (path: string) => {
    if (!path || loading) return;
    setLoading(true);
    try {
      const result = await skillsApi.load(requestScope, path);
      assertActiveScope(requestScope);
      const command = skillCommandReceipt(result);
      if (command.action !== 'installed') {
        throw new Error(`Unexpected Skill receipt '${command.action}' after install`);
      }
      await refreshSkills();
      setDir('');
      showSettlement(
        '技能安装',
        command.settlement.settlement,
        `已安装并启用 ${command.settlement.name}`
      );
    } catch (e: any) {
      const msg = e?.message || String(e);
      addToast('error', `加载技能失败: ${msg}`);
    } finally {
      setLoading(false);
    }
  };

  const load = async () => {
    await loadPath(dir.trim());
  };

  const enableSkill = async (name: string) => {
    if (busySkill) return;
    setBusySkill(name);
    try {
      const result = await skillsApi.enable(requestScope, name);
      assertActiveScope(requestScope);
      const command = skillCommandReceipt(result);
      if (command.action !== 'enabled') {
        throw new Error(`Unexpected Skill receipt '${command.action}' after enable`);
      }
      await refreshSkills();
      showSettlement(
        '技能启用',
        command.settlement,
        command.settlement.idempotent ? `${name} 已是启用状态` : `已启用 ${name}`
      );
    } catch (e: any) {
      addToast('error', `启用技能失败: ${e?.message || String(e)}`);
    } finally {
      setBusySkill(null);
    }
  };

  const disableSkill = async (name: string) => {
    if (busySkill) return;
    setBusySkill(name);
    try {
      const result = await skillsApi.disable(requestScope, name);
      assertActiveScope(requestScope);
      const command = skillCommandReceipt(result);
      if (command.action !== 'disabled') {
        throw new Error(`Unexpected Skill receipt '${command.action}' after disable`);
      }
      await refreshSkills();
      showSettlement(
        '技能禁用',
        command.settlement,
        command.settlement.idempotent ? `${name} 已是禁用状态` : `已禁用 ${name}`
      );
    } catch (e: any) {
      addToast('error', `禁用技能失败: ${e?.message || String(e)}`);
    } finally {
      setBusySkill(null);
    }
  };

  const uninstallSkill = async (name: string) => {
    if (busySkill || !window.confirm(`卸载技能 '${name}'？`)) return;
    setBusySkill(name);
    try {
      const result = await skillsApi.uninstall(requestScope, name);
      assertActiveScope(requestScope);
      const command = skillCommandReceipt(result);
      if (command.action !== 'uninstalled') {
        throw new Error(`Unexpected Skill receipt '${command.action}' after uninstall`);
      }
      await refreshSkills();
      showSettlement('技能卸载', command.settlement.settlement, `已卸载 ${name}`);
    } catch (e: any) {
      addToast('error', `卸载技能失败: ${e?.message || String(e)}`);
    } finally {
      setBusySkill(null);
    }
  };

  const checkUpdates = async () => {
    if (loadingAny) return;
    setCheckingUpdates(true);
    try {
      const receipt = await skillsApi.checkUpdates(requestScope);
      assertActiveScope(requestScope);
      const command = skillCommandReceipt(receipt);
      if (command.action !== 'updates_checked') {
        throw new Error(`Unexpected Skill receipt '${command.action}' after update check`);
      }
      const statuses = command.updates.items;
      setUpdateStatuses(Object.fromEntries(statuses.map((status) => [status.name, status])));
      setUpdatesOmitted(command.updates.omitted);
      const available = statuses.filter((status) => status.state === 'update_available').length;
      const localChanges = statuses.filter((status) => status.state === 'local_changes').length;
      addToast(
        available > 0 || localChanges > 0 ? 'info' : 'success',
        `检查完成：${available} 个更新，${localChanges} 个存在本地修改${command.updates.omitted > 0 ? `，${command.updates.omitted} 项未展示` : ''}`
      );
    } catch (e: any) {
      addToast('error', `检查技能更新失败: ${e?.message || String(e)}`);
    } finally {
      setCheckingUpdates(false);
    }
  };

  const syncSkill = async (name: string, force = false) => {
    if (loadingAny || busySkill) return;
    setBusySkill(name);
    setSyncingSkills(true);
    try {
      const receipt = await skillsApi.sync(requestScope, name, force);
      assertActiveScope(requestScope);
      const command = skillCommandReceipt(receipt);
      if (command.action !== 'synced') {
        throw new Error(`Unexpected Skill receipt '${command.action}' after sync`);
      }
      const result = command.settlement.results.find((candidate) => candidate.name === name);
      await refreshSkills();
      const updateReceipt = await skillsApi.checkUpdates(requestScope);
      assertActiveScope(requestScope);
      const updateCommand = skillCommandReceipt(updateReceipt);
      if (updateCommand.action !== 'updates_checked') {
        throw new Error(`Unexpected Skill receipt '${updateCommand.action}' after update check`);
      }
      setUpdateStatuses(
        Object.fromEntries(updateCommand.updates.items.map((status) => [status.name, status]))
      );
      setUpdatesOmitted(updateCommand.updates.omitted);
      if (result?.success === false) {
        addToast('error', result.message);
      } else {
        showSettlement(
          '技能同步',
          command.settlement.settlement,
          result?.message || (result?.updated ? `已更新 ${name}` : `${name} 无需更新`)
        );
      }
    } catch (e: any) {
      addToast('error', `同步技能失败: ${e?.message || String(e)}`);
    } finally {
      setBusySkill(null);
      setSyncingSkills(false);
    }
  };

  const refreshEnabledSkills = async () => {
    if (loadingAny) return;
    setLoading(true);
    try {
      const receipt = await skillsApi.refresh(requestScope);
      assertActiveScope(requestScope);
      const command = skillCommandReceipt(receipt);
      if (command.action !== 'refreshed') {
        throw new Error(`Unexpected Skill receipt '${command.action}' after refresh`);
      }
      await refreshSkills();
      showSettlement('技能刷新', command.settlement, '已刷新当前 workspace 的技能运行时');
    } catch (e: any) {
      addToast('error', `刷新技能失败: ${e?.message || String(e)}`);
    } finally {
      setLoading(false);
    }
  };

  const handleBrowse = async () => {
    if (loadingAny) return;

    if (isTauri()) {
      setUploading(true);
      try {
        const selected = await fileSystem.selectDirectory('选择技能目录');
        if (!selected) return;
        setDir(selected);
        await loadPath(selected);
      } catch (e: any) {
        const msg = e?.message || String(e);
        addToast('error', `选择技能目录失败: ${msg}`);
      } finally {
        setUploading(false);
      }
      return;
    }

    fileInputRef.current?.click();
  };

  const handleDirPick = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const fileList = e.target.files;
    if (!fileList || fileList.length === 0) return;

    setUploading(true);
    try {
      // 收集 .md/.markdown 文件
      const mdFiles: { path: string; content: string }[] = [];
      const rootDir = fileList[0].webkitRelativePath.split('/')[0] || 'skills';

      for (let i = 0; i < fileList.length; i++) {
        const file = fileList[i];
        if (file.name.endsWith('.md') || file.name.endsWith('.markdown')) {
          const content = await readFileAsText(file);
          mdFiles.push({ path: file.webkitRelativePath, content });
        }
      }

      if (mdFiles.length === 0) {
        addToast('error', '所选目录中没有找到 .md 技能文件');
        return;
      }

      const result = await skillsApi.upload(rootDir, mdFiles);
      setSkills(result.skills);
      addToast('success', result.message || `成功上传 ${result.loaded.length} 个技能`);
    } catch (e: any) {
      const msg = e?.message || String(e);
      addToast('error', `上传技能失败: ${msg}`);
    } finally {
      setUploading(false);
      // 重置 input 以便可以重新选择相同目录
      if (fileInputRef.current) fileInputRef.current.value = '';
    }
  };

  const s = {
    text: 'var(--text-primary)',
    textSec: 'var(--text-secondary)',
    textTer: 'var(--text-tertiary)',
    border: 'var(--border-primary)',
    bg: 'var(--bg-primary)',
    bgInput: 'var(--bg-input)',
    accent: 'var(--accent)',
  };

  return (
    <div className="p-3 space-y-3">
      <div className="flex items-center justify-between gap-2">
        <h3 className="text-sm font-semibold" style={{ color: s.text }}>
          技能 ({skills.length}
          {skillsOmitted > 0 ? ` + ${skillsOmitted}` : ''})
        </h3>
        <div className="flex items-center gap-1">
          <button
            type="button"
            onClick={refreshEnabledSkills}
            disabled={loadingAny}
            className="flex h-7 w-7 items-center justify-center rounded-md border disabled:opacity-50"
            style={{ borderColor: s.border, color: s.textSec }}
            title="刷新当前 workspace 的技能运行时"
            aria-label="刷新当前 workspace 的技能运行时"
          >
            <RefreshCw size={11} className={loading ? 'animate-spin' : ''} />
          </button>
          <button
            type="button"
            onClick={checkUpdates}
            disabled={loadingAny}
            className="flex h-7 items-center gap-1 rounded-md border px-2 text-[10px] disabled:opacity-50"
            style={{ borderColor: s.border, color: s.textSec }}
            title="检查 Git 安装技能的上游更新"
          >
            <RefreshCw size={11} className={checkingUpdates ? 'animate-spin' : ''} />
            检查更新{updatesOmitted > 0 ? ` (+${updatesOmitted})` : ''}
          </button>
        </div>
      </div>

      <div className="flex gap-2">
        <input
          value={dir}
          onChange={(e) => setDir(e.target.value)}
          className="flex-1 rounded-lg border px-2 py-1.5 text-xs"
          style={{ background: s.bgInput, borderColor: s.border, color: s.text }}
          placeholder="技能目录路径"
        />
        <button
          onClick={load}
          disabled={!dir.trim() || loadingAny}
          className="flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-medium text-white transition-opacity disabled:opacity-50"
          style={{ background: s.accent }}
        >
          {loading ? <Loader2 size={14} className="animate-spin" /> : <FolderOpen size={14} />}
          加载
        </button>
        <button
          onClick={handleBrowse}
          disabled={loadingAny}
          className="flex items-center gap-1.5 rounded-lg border px-3 py-1.5 text-xs font-medium transition-opacity disabled:opacity-50"
          style={{ borderColor: s.border, color: s.text }}
        >
          {uploading ? <Loader2 size={14} className="animate-spin" /> : <FolderOpen size={14} />}
          浏览
        </button>
        <input
          ref={fileInputRef}
          type="file"
          // @ts-expect-error webkitdirectory is not in React types
          webkitdirectory=""
          directory=""
          onChange={handleDirPick}
          className="hidden"
        />
      </div>

      <div className="relative">
        <Search
          size={13}
          className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2"
          style={{ color: s.textTer }}
        />
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          className="w-full rounded-lg border py-1.5 pl-7 pr-2 text-xs"
          style={{ background: s.bgInput, borderColor: s.border, color: s.text }}
          placeholder="搜索技能"
        />
      </div>

      {filteredSkills.length === 0 && (
        <div className="py-8 text-center text-xs" style={{ color: s.textTer }}>
          <BookOpen size={24} className="mx-auto mb-2" />
          {query.trim() ? '没有匹配的技能' : '暂无可用技能'}
        </div>
      )}

      {groupedSkills.map(([cat, catSkills]) => {
        const enabledCount = catSkills.filter((s) => s.loaded).length;
        const catLabel = CATEGORY_LABELS[cat] || cat;
        const collapsed = collapsedCategories.has(cat);
        return (
          <div key={cat}>
            <button
              onClick={() => toggleCategory(cat)}
              className="flex w-full items-center gap-1.5 rounded-lg px-2 py-1.5 text-xs font-medium transition-colors hover:opacity-80"
              style={{ color: s.text, background: 'var(--bg-hover)' }}
            >
              {collapsed ? <ChevronRight size={12} /> : <ChevronDown size={12} />}
              <span>{catLabel}</span>
              <span className="ml-auto text-[10px]" style={{ color: s.textTer }}>
                {enabledCount}/{catSkills.length}
              </span>
            </button>
            {!collapsed &&
              catSkills.map((sk) => (
                <div
                  key={sk.name}
                  className="ml-2 mt-1 rounded-lg border px-3 py-2"
                  style={{ borderColor: s.border, background: s.bg }}
                >
                  <div className="flex items-start gap-2">
                    {sk.is_baseline ? (
                      <Star size={12} className="mt-0.5 shrink-0" style={{ color: '#eab308' }} />
                    ) : (
                      <BookOpen size={12} className="mt-0.5 shrink-0" style={{ color: s.accent }} />
                    )}
                    <div className="min-w-0 flex-1">
                      <div className="flex min-w-0 items-center gap-1.5">
                        <span className="truncate text-xs font-medium" style={{ color: s.text }}>
                          {sk.name}
                        </span>
                        {sk.is_baseline && (
                          <span
                            className="shrink-0 rounded-md px-1 py-0.5 text-[8px] font-medium"
                            style={{ background: '#eab30820', color: '#eab308' }}
                          >
                            baseline
                          </span>
                        )}
                        {sk.missing_dependencies && sk.missing_dependencies.length > 0 && (
                          <span title={sk.missing_dependencies.join(', ')}>
                            <AlertTriangle
                              size={10}
                              className="shrink-0"
                              style={{ color: '#f59e0b' }}
                            />
                          </span>
                        )}
                        <span
                          className="shrink-0 rounded-md px-1.5 py-0.5 text-[9px]"
                          style={{
                            background: sk.loaded ? 'var(--accent-muted)' : 'var(--bg-hover)',
                            color: sk.loaded ? s.accent : s.textTer,
                          }}
                        >
                          {sk.loaded ? '已接入' : '可用'}
                        </span>
                      </div>
                      <p className="mt-1 text-xs" style={{ color: s.textSec }}>
                        {sk.description || '无描述'}
                      </p>
                      {(sk.upstream_version || sk.source) && (
                        <div className="mt-0.5 flex gap-2 text-[9px]" style={{ color: s.textTer }}>
                          {sk.source && <span>{sk.source}</span>}
                          {sk.upstream_version && <span>· v{sk.upstream_version}</span>}
                        </div>
                      )}
                    </div>
                    <button
                      onClick={() => (sk.loaded ? disableSkill(sk.name) : enableSkill(sk.name))}
                      disabled={Boolean(busySkill)}
                      className="flex shrink-0 items-center gap-1 rounded-md border px-2 py-1 text-[10px] transition-opacity disabled:opacity-50"
                      style={{ borderColor: s.border, color: s.text }}
                      title={sk.loaded ? '禁用技能' : '启用技能'}
                    >
                      {busySkill === sk.name ? (
                        <Loader2 size={11} className="animate-spin" />
                      ) : (
                        <Power size={11} />
                      )}
                      {sk.loaded ? '禁用' : '启用'}
                    </button>
                    {!sk.is_builtin && (
                      <button
                        onClick={() => void uninstallSkill(sk.name)}
                        disabled={Boolean(busySkill)}
                        className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md border transition-opacity disabled:opacity-50"
                        style={{ borderColor: s.border, color: 'var(--color-error)' }}
                        title="卸载技能"
                        aria-label={`卸载技能 ${sk.name}`}
                      >
                        <Trash2 size={11} />
                      </button>
                    )}
                  </div>
                  {updateStatuses[sk.name] && (
                    <div
                      className="mt-2 flex items-center gap-2 border-t pt-2 text-[10px]"
                      style={{ borderColor: s.border, color: s.textTer }}
                    >
                      <span
                        className="min-w-0 flex-1 truncate"
                        title={updateStatuses[sk.name].message}
                      >
                        {updateStatuses[sk.name].message}
                      </span>
                      {(updateStatuses[sk.name].state === 'update_available' ||
                        updateStatuses[sk.name].state === 'local_changes') && (
                        <button
                          type="button"
                          onClick={() =>
                            syncSkill(sk.name, updateStatuses[sk.name].state === 'local_changes')
                          }
                          disabled={loadingAny || Boolean(busySkill)}
                          className="flex h-6 shrink-0 items-center gap-1 rounded-md border px-2 disabled:opacity-50"
                          style={{ borderColor: s.border, color: s.accent }}
                          title={
                            updateStatuses[sk.name].state === 'local_changes'
                              ? '覆盖本地修改并同步上游版本'
                              : '同步上游版本'
                          }
                        >
                          {busySkill === sk.name ? (
                            <Loader2 size={10} className="animate-spin" />
                          ) : (
                            <Download size={10} />
                          )}
                          {updateStatuses[sk.name].state === 'local_changes' ? '强制同步' : '同步'}
                        </button>
                      )}
                    </div>
                  )}
                  {sk.tags && sk.tags.length > 0 && (
                    <div className="mt-1.5 flex flex-wrap gap-1">
                      {sk.tags.map((tag) => (
                        <span
                          key={tag}
                          className="rounded-md px-1.5 py-0.5 text-[9px]"
                          style={{ background: 'var(--bg-hover)', color: s.textTer }}
                        >
                          {tag}
                        </span>
                      ))}
                    </div>
                  )}
                  {sk.path && (
                    <p className="mt-1 text-[10px]" style={{ color: s.textTer }}>
                      {sk.path}
                    </p>
                  )}
                </div>
              ))}
          </div>
        );
      })}
    </div>
  );
}

function readFileAsText(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result as string);
    reader.onerror = () => reject(reader.error);
    reader.readAsText(file);
  });
}
