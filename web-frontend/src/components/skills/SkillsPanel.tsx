import { useEffect, useRef, useState } from 'react';
import { skillsApi } from '../../api/endpoints';
import type { SkillInfo } from '../../types/api';
import { BookOpen, FolderOpen, Loader2 } from 'lucide-react';
import { useToastStore } from '../../stores/toastStore';

export function SkillsPanel() {
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [dir, setDir] = useState('');
  const [loading, setLoading] = useState(false);
  const [uploading, setUploading] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const addToast = useToastStore((s) => s.addToast);

  useEffect(() => {
    skillsApi.list().then(setSkills).catch(console.error);
  }, []);

  const load = async () => {
    if (!dir.trim() || loading) return;
    setLoading(true);
    try {
      await skillsApi.load(dir.trim());
      setDir('');
      const list = await skillsApi.list();
      setSkills(list);
      addToast('success', `成功加载 ${list.length} 个技能`);
    } catch (e: any) {
      const msg = e?.message || String(e);
      addToast('error', `加载技能失败: ${msg}`);
    } finally {
      setLoading(false);
    }
  };

  const handleBrowse = () => {
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

  const loadingAny = loading || uploading;

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

      {skills.length === 0 && (
        <div className="py-8 text-center text-xs" style={{ color: s.textTer }}>
          <BookOpen size={24} className="mx-auto mb-2" />
          暂无加载的技能
        </div>
      )}

      {skills.map((sk) => (
        <div
          key={sk.name}
          className="rounded-lg border px-3 py-2"
          style={{ borderColor: s.border, background: s.bg }}
        >
          <div className="flex items-center gap-2">
            <BookOpen size={12} style={{ color: s.accent }} />
            <span className="text-xs font-medium" style={{ color: s.text }}>
              {sk.name}
            </span>
          </div>
          <p className="mt-1 text-xs" style={{ color: s.textSec }}>
            {sk.description}
          </p>
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
