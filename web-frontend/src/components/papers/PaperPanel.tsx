import { useState, useCallback, useEffect } from 'react';
import { papersApi, type Paper, type CreatePaperRequest } from '../../api/endpoints';
import { BookOpen, Plus, X, Grid3X3, List } from 'lucide-react';
import { PaperList } from './PaperList';
import { PaperDetail } from './PaperDetail';
import { ReviewMatrix } from './ReviewMatrix';

type ViewMode = 'list' | 'matrix';

export function PaperPanel() {
  const [papers, setPapers] = useState<Paper[]>([]);
  const [selectedPaper, setSelectedPaper] = useState<Paper | null>(null);
  const [viewMode, setViewMode] = useState<ViewMode>('list');
  const [showAddForm, setShowAddForm] = useState(false);
  const [refreshKey, setRefreshKey] = useState(0);

  // Add form state
  const [addTitle, setAddTitle] = useState('');
  const [addAuthors, setAddAuthors] = useState('');
  const [addYear, setAddYear] = useState('');
  const [addVenue, setAddVenue] = useState('');
  const [addArxiv, setAddArxiv] = useState('');
  const [addTags, setAddTags] = useState('');
  const [addAbstract, setAddAbstract] = useState('');
  const [saving, setSaving] = useState(false);

  const refresh = useCallback(() => {
    setRefreshKey((k) => k + 1);
  }, []);

  // Fetch papers on mount and refresh
  const fetchPapers = useCallback(async () => {
    try {
      const list = await papersApi.list();
      setPapers(list);
    } catch (e) {
      console.error('Failed to fetch papers:', e);
    }
  }, []);

  useEffect(() => {
    fetchPapers();
  }, [fetchPapers, refreshKey]);

  const handleSelectPaper = async (paper: Paper) => {
    // Fetch full details
    try {
      const full = await papersApi.get(paper.id);
      setSelectedPaper(full);
    } catch {
      setSelectedPaper(paper);
    }
  };

  const handleAddPaper = async () => {
    if (!addTitle.trim()) return;
    setSaving(true);
    try {
      const req: CreatePaperRequest = {
        title: addTitle.trim(),
        authors: addAuthors
          ? addAuthors
              .split(',')
              .map((a) => a.trim())
              .filter(Boolean)
          : undefined,
        year: addYear ? parseInt(addYear, 10) : undefined,
        venue: addVenue || undefined,
        arxiv_id: addArxiv || undefined,
        tags: addTags
          ? addTags
              .split(',')
              .map((t) => t.trim())
              .filter(Boolean)
          : undefined,
        abstract_text: addAbstract || undefined,
      };
      const created = await papersApi.create(req);
      setSelectedPaper(created);
      setShowAddForm(false);
      setAddTitle('');
      setAddAuthors('');
      setAddYear('');
      setAddVenue('');
      setAddArxiv('');
      setAddTags('');
      setAddAbstract('');
      refresh();
    } catch (e) {
      console.error('Failed to add paper:', e);
    } finally {
      setSaving(false);
    }
  };

  const s = {
    text: 'var(--text-primary)',
    textSec: 'var(--text-secondary)',
    textTer: 'var(--text-tertiary)',
    border: 'var(--border-primary)',
    bg: 'var(--bg-primary)',
    bgHover: 'var(--bg-hover)',
    bgCard: 'var(--bg-secondary)',
  };

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div
        className="flex items-center justify-between px-3 py-2.5"
        style={{ borderBottom: `1px solid ${s.border}` }}
      >
        <h3 className="text-sm font-semibold flex items-center gap-1.5" style={{ color: s.text }}>
          <BookOpen size={14} />
          Papers
        </h3>
        <div className="flex items-center gap-1">
          {/* View mode toggle */}
          <button
            onClick={() => setViewMode('list')}
            className="rounded p-1.5 transition-colors"
            style={{ color: viewMode === 'list' ? 'var(--color-primary)' : s.textTer }}
            title="List view"
          >
            <List size={13} />
          </button>
          <button
            onClick={() => setViewMode('matrix')}
            className="rounded p-1.5 transition-colors"
            style={{ color: viewMode === 'matrix' ? 'var(--color-primary)' : s.textTer }}
            title="Matrix view"
          >
            <Grid3X3 size={13} />
          </button>
          <button
            onClick={() => setShowAddForm(true)}
            className="rounded p-1.5 transition-colors"
            style={{ color: s.textTer }}
            title="Add paper"
          >
            <Plus size={13} />
          </button>
        </div>
      </div>

      {/* Add paper form */}
      {showAddForm && (
        <div
          className="p-3 space-y-2"
          style={{ borderBottom: `1px solid ${s.border}`, background: s.bgCard }}
        >
          <div className="flex items-center justify-between mb-1">
            <span className="text-xs font-medium" style={{ color: s.text }}>
              Add Paper
            </span>
            <button
              onClick={() => setShowAddForm(false)}
              className="rounded p-1"
              style={{ color: s.textTer }}
            >
              <X size={12} />
            </button>
          </div>
          <input
            type="text"
            value={addTitle}
            onChange={(e) => setAddTitle(e.target.value)}
            placeholder="Title *"
            className="w-full rounded border px-2 py-1 text-xs"
            style={{ borderColor: s.border, background: s.bg, color: s.text }}
          />
          <input
            type="text"
            value={addAuthors}
            onChange={(e) => setAddAuthors(e.target.value)}
            placeholder="Authors (comma-separated)"
            className="w-full rounded border px-2 py-1 text-xs"
            style={{ borderColor: s.border, background: s.bg, color: s.text }}
          />
          <div className="grid grid-cols-2 gap-2">
            <input
              type="text"
              value={addYear}
              onChange={(e) => setAddYear(e.target.value)}
              placeholder="Year"
              className="rounded border px-2 py-1 text-xs"
              style={{ borderColor: s.border, background: s.bg, color: s.text }}
            />
            <input
              type="text"
              value={addVenue}
              onChange={(e) => setAddVenue(e.target.value)}
              placeholder="Venue"
              className="rounded border px-2 py-1 text-xs"
              style={{ borderColor: s.border, background: s.bg, color: s.text }}
            />
          </div>
          <input
            type="text"
            value={addArxiv}
            onChange={(e) => setAddArxiv(e.target.value)}
            placeholder="arXiv ID (e.g. 2301.12345)"
            className="w-full rounded border px-2 py-1 text-xs"
            style={{ borderColor: s.border, background: s.bg, color: s.text }}
          />
          <input
            type="text"
            value={addTags}
            onChange={(e) => setAddTags(e.target.value)}
            placeholder="Tags (comma-separated)"
            className="w-full rounded border px-2 py-1 text-xs"
            style={{ borderColor: s.border, background: s.bg, color: s.text }}
          />
          <textarea
            value={addAbstract}
            onChange={(e) => setAddAbstract(e.target.value)}
            placeholder="Abstract (optional)"
            className="w-full rounded border px-2 py-1 text-xs"
            style={{ borderColor: s.border, background: s.bg, color: s.text }}
            rows={3}
          />
          <button
            onClick={handleAddPaper}
            disabled={!addTitle.trim() || saving}
            className="w-full rounded px-3 py-1.5 text-xs font-medium transition-colors"
            style={{
              background: addTitle.trim() ? 'var(--color-primary)' : s.bgHover,
              color: addTitle.trim() ? '#fff' : s.textTer,
            }}
          >
            {saving ? 'Adding...' : 'Add Paper'}
          </button>
        </div>
      )}

      {/* Content */}
      <div className="flex-1 overflow-hidden">
        {viewMode === 'matrix' ? (
          <div className="h-full overflow-y-auto">
            <ReviewMatrix />
          </div>
        ) : selectedPaper ? (
          <div className="h-full flex flex-col md:flex-row">
            {/* List side */}
            <div
              className="w-full md:w-1/2 h-1/2 md:h-full overflow-hidden"
              style={{ borderRight: `1px solid ${s.border}` }}
            >
              <PaperList
                papers={papers}
                selectedId={selectedPaper.id}
                onSelect={handleSelectPaper}
              />
            </div>
            {/* Detail side */}
            <div className="w-full md:w-1/2 h-1/2 md:h-full overflow-y-auto">
              <PaperDetail
                paper={selectedPaper}
                onClose={() => setSelectedPaper(null)}
                onUpdated={refresh}
              />
            </div>
          </div>
        ) : (
          <PaperList papers={papers} selectedId={null} onSelect={handleSelectPaper} />
        )}
      </div>
    </div>
  );
}

export default PaperPanel;
