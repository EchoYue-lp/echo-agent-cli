import { useEffect, useState } from 'react';
import { skillsApi } from '../../api/endpoints';
import type { SkillInfo } from '../../types/api';
import { BookOpen, FolderOpen } from 'lucide-react';

export function SkillsPanel() {
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [dir, setDir] = useState('');

  useEffect(() => {
    skillsApi.list().then(setSkills).catch(console.error);
  }, []);

  const load = async () => {
    try {
      await skillsApi.load(dir);
      setDir('');
      skillsApi.list().then(setSkills);
    } catch (e) {
      console.error(e);
    }
  };

  return (
    <div className="p-3 space-y-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-gray-700">Skills ({skills.length})</h3>
      </div>

      <div className="flex gap-2">
        <input
          value={dir}
          onChange={(e) => setDir(e.target.value)}
          className="flex-1 rounded border px-2 py-1 text-sm"
          placeholder="Skills directory path"
        />
        <button onClick={load} className="rounded bg-indigo-600 px-3 py-1 text-sm text-white hover:bg-indigo-700">
          <FolderOpen size={14} />
        </button>
      </div>

      {skills.map((s) => (
        <div key={s.name} className="rounded border border-gray-200 bg-white px-3 py-2">
          <div className="flex items-center gap-2">
            <BookOpen size={12} className="text-blue-500" />
            <span className="text-xs font-medium">{s.name}</span>
          </div>
          <p className="mt-1 text-xs text-gray-500">{s.description}</p>
          <p className="mt-1 text-[10px] text-gray-400">{s.file}</p>
        </div>
      ))}
    </div>
  );
}
