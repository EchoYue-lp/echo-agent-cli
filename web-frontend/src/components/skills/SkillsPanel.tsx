import { useEffect, useMemo, useRef, useState } from 'react';
import { skillsApi } from '../../api/endpoints';
import type { SkillInfo } from '../../types/api';
import { CATEGORY_LABELS } from '../../types/api';
import { BookOpen, ChevronDown, ChevronRight, FolderOpen, Loader2, Power, Search, Star, AlertTriangle } from 'lucide-react';
import { useToastStore } from '../../stores/toastStore';
import { fileSystem, isTauri } from '../../lib/tauri-bridge';

/** Category sort order for consistent display. */
const CATEGORY_ORDER = ['methodology', 'development', 'document', 'design', 'research', 'automation'];

export function SkillsPanel() {
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [dir, setDir] = useState('');
  const [loading, setLoading] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [query, setQuery] = useState('');
  const [busySkill, setBusySkill] = useState<string | null>(null);
  const [collapsedCategories, setCollapsedCategories] = useState<Set<string>>(new Set());
  const fileInputRef = useRef<HTMLInputElement>(null);
  const addToast = useToastStore((s) => s.addToast);
  const loadingAny = loading || uploading;

  const filteredSkills = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return skills;
    return skills.filter((skill) => {
      const catLabel = CATEGORY_LABELS[skill.category || ''] || '';
      return (
        skill.name.toLowerCase().includes(q) ||
        skill.description.toLowerCase().includes(q) ||
        skill.tags?.some((tag) => tag.toLowerCase().includes(q)) ||
        catLabel.includes(q) ||
        (skill.category || '').toLowerCase().includes(q)
      );
    });
  }, [skills, query]);

  /** Group filtered skills by category, maintaining display order. */
  const groupedSkills = useMemo(() => {
    const groups: Record<string, SkillInfo[]> = {};
    for (const sk of filteredSkills) {
      const cat = sk.category || 'other';
      if (!groups[cat]) groups[cat] = [];
      groups[cat].push(sk);
    }
    // Sort categories
    const sorted: [string, SkillInfo[]][] = [];
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
      if (next.has(cat)) next.delete(cat); else next.add(cat);
      return next;
    });
  };

  useEffect(() => {
    refreshSkills().catch(console.error);
  }, []);

  async function refreshSkills() {
    const list = await skillsApi.list();
    setSkills(list);
    return list;
  }

  const loadPath = async (path: string) => {
    if (!path || loading) return;
    setLoading(true);
    try {
      const result = await skillsApi.load(path);
      const list = result.skills ?? (await refreshSkills());
      setSkills(list);
      setDir('');
      const loadedCount = result.count ?? result.loaded?.length ?? list.length;
      addToast('success', loadedCount > 0 ? `成功加载 ${loadedCount} 个技能` : '未发现新的技能');
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
      const result = await skillsApi.enable(name);
      setSkills(result.skills ?? (await refreshSkills()));
      const loadedCount = result.count ?? result.loaded?.length ?? 0;
      addToast('success', loadedCount > 0 ? `已启用 ${name}` : `${name} 已是可用状态`);
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
      const result = await skillsApi.disable(name);
      if (result.skills) setSkills(result.skills);
      else await refreshSkills();
      addToast(result.requires_restart ? 'info' : 'success', result.message || `已禁用 ${name}`);
    } catch (e: any) {
      addToast('error', `禁用技能失败: ${e?.message || String(e)}`);
    } finally {
      setBusySkill(null);
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
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold" style={{ color: s.text }}>
          技能 ({skills.length})
        </h3>
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
          {skills.length === 0 ? '暂无可用技能' : '没有匹配的技能'}
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
            {!collapsed && catSkills.map((sk) => (
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
                          className="shrink-0 rounded px-1 py-0.5 text-[8px] font-medium"
                          style={{ background: '#eab30820', color: '#eab308' }}
                        >
                          baseline
                        </span>
                      )}
                      {sk.missing_dependencies && sk.missing_dependencies.length > 0 && (
                        <AlertTriangle size={10} className="shrink-0" style={{ color: '#f59e0b' }} title={sk.missing_dependencies.join(', ')} />
                      )}
                      <span
                        className="shrink-0 rounded px-1.5 py-0.5 text-[9px]"
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
                    {busySkill === sk.name ? <Loader2 size={11} className="animate-spin" /> : <Power size={11} />}
                    {sk.loaded ? '禁用' : '启用'}
                  </button>
                </div>
                {sk.tags && sk.tags.length > 0 && (
                  <div className="mt-1.5 flex flex-wrap gap-1">
              {sk.tags.map((tag) => (
                <span
                  key={tag}
                  className="rounded px-1.5 py-0.5 text-[9px]"
                  style={{ background: 'var(--bg-hover)', color: s.textTer }}
                >
                  {tag}
                </span>
              ))}
            </div>
          )}
          {sk.file && (
            <p className="mt-1 text-[10px]" style={{ color: s.textTer }}>
              {sk.file}
            </p>
          )}
        </div>
      ))}
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
