import { useCallback, useEffect, useState } from 'react';
import { papersApi, type CreatePaperRequest, type Paper } from '../../api/endpoints';
import { BookOpen, Grid3X3, List, Plus, SearchCheck, X } from 'lucide-react';
import { PaperDetail } from './PaperDetail';
import { PaperList } from './PaperList';
import { ReviewMatrix } from './ReviewMatrix';
import { ReviewWorkbench } from './ReviewWorkbench';

type ViewMode = 'library' | 'matrix' | 'reviews';

export function PaperPanel() {
  const [papers, setPapers] = useState<Paper[]>([]);
  const [selectedPaper, setSelectedPaper] = useState<Paper | null>(null);
  const [viewMode, setViewMode] = useState<ViewMode>('library');
  const [showAddForm, setShowAddForm] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState<CreatePaperRequest>({
    title: '',
    source_kind: 'journal_article',
  });

  const fetchPapers = useCallback(async () => {
    try {
      const list = await papersApi.list();
      setPapers(list);
      setSelectedPaper((current) => list.find((paper) => paper.id === current?.id) ?? null);
      setError(null);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }, []);

  useEffect(() => {
    void fetchPapers();
  }, [fetchPapers]);

  const handleSelectPaper = async (paper: Paper) => {
    try {
      setSelectedPaper(await papersApi.get(paper.id));
    } catch {
      setSelectedPaper(paper);
    }
  };

  const handleAddPaper = async () => {
    if (!draft.title.trim()) return;
    setSaving(true);
    try {
      const created = await papersApi.create({
        ...draft,
        title: draft.title.trim(),
        authors: draft.authors?.filter(Boolean),
        tags: draft.tags?.filter(Boolean),
      });
      setSelectedPaper(created);
      setShowAddForm(false);
      setDraft({ title: '', source_kind: 'journal_article' });
      await fetchPapers();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex min-h-10 items-center gap-1 border-b border-[var(--border-primary)] px-2">
        <PanelMode
          active={viewMode === 'library'}
          icon={<List size={12} />}
          label="Library"
          onClick={() => setViewMode('library')}
        />
        <PanelMode
          active={viewMode === 'matrix'}
          icon={<Grid3X3 size={12} />}
          label="Matrix"
          onClick={() => setViewMode('matrix')}
        />
        <PanelMode
          active={viewMode === 'reviews'}
          icon={<SearchCheck size={12} />}
          label="Reviews"
          onClick={() => setViewMode('reviews')}
        />
        <div className="flex-1" />
        {viewMode === 'library' && (
          <button
            type="button"
            onClick={() => setShowAddForm((visible) => !visible)}
            className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            title="Add source"
          >
            {showAddForm ? <X size={13} /> : <Plus size={13} />}
          </button>
        )}
      </header>

      {error && (
        <div className="border-b border-[var(--border-primary)] bg-red-500/10 px-3 py-2 text-xs text-red-500">
          {error}
        </div>
      )}

      {showAddForm && viewMode === 'library' && (
        <SourceForm
          draft={draft}
          setDraft={setDraft}
          save={() => void handleAddPaper()}
          saving={saving}
        />
      )}

      <div className="min-h-0 flex-1">
        {viewMode === 'matrix' ? (
          <ReviewMatrix />
        ) : viewMode === 'reviews' ? (
          <ReviewWorkbench />
        ) : selectedPaper ? (
          <div className="flex h-full min-h-0 flex-col md:flex-row">
            <div className="h-2/5 min-h-0 w-full border-b border-[var(--border-primary)] md:h-full md:w-2/5 md:border-b-0 md:border-r">
              <PaperList
                papers={papers}
                selectedId={selectedPaper.id}
                onSelect={(paper) => void handleSelectPaper(paper)}
              />
            </div>
            <div className="min-h-0 flex-1">
              <PaperDetail
                paper={selectedPaper}
                onClose={() => setSelectedPaper(null)}
                onUpdated={() => void fetchPapers()}
              />
            </div>
          </div>
        ) : (
          <PaperList
            papers={papers}
            selectedId={null}
            onSelect={(paper) => void handleSelectPaper(paper)}
          />
        )}
      </div>
    </div>
  );
}

function SourceForm({
  draft,
  setDraft,
  save,
  saving,
}: {
  draft: CreatePaperRequest;
  setDraft: React.Dispatch<React.SetStateAction<CreatePaperRequest>>;
  save: () => void;
  saving: boolean;
}) {
  const set = (patch: Partial<CreatePaperRequest>) =>
    setDraft((current) => ({ ...current, ...patch }));
  return (
    <div className="space-y-2 border-b border-[var(--border-primary)] bg-[var(--bg-secondary)] p-3">
      <div className="flex items-center gap-1.5 text-xs font-semibold text-[var(--text-primary)]">
        <BookOpen size={12} /> Add source
      </div>
      <input
        value={draft.title}
        onChange={(event) => set({ title: event.target.value })}
        className={inputClass}
        placeholder="Title"
      />
      <input
        value={draft.authors?.join(', ') ?? ''}
        onChange={(event) => set({ authors: splitComma(event.target.value) })}
        className={inputClass}
        placeholder="Authors, comma separated"
      />
      <div className="grid grid-cols-2 gap-2 md:grid-cols-4">
        <select
          value={draft.source_kind ?? 'journal_article'}
          onChange={(event) => set({ source_kind: event.target.value as Paper['source_kind'] })}
          className={inputClass}
        >
          <option value="journal_article">Journal article</option>
          <option value="preprint">Preprint</option>
          <option value="conference_paper">Conference paper</option>
          <option value="guideline">Guideline</option>
          <option value="trial_registration">Trial registration</option>
          <option value="dataset">Dataset</option>
          <option value="other">Other</option>
        </select>
        <input
          type="number"
          value={draft.year ?? ''}
          onChange={(event) =>
            set({ year: event.target.value ? Number(event.target.value) : undefined })
          }
          className={inputClass}
          placeholder="Year"
        />
        <input
          value={draft.venue ?? ''}
          onChange={(event) => set({ venue: event.target.value || undefined })}
          className={inputClass}
          placeholder="Venue"
        />
        <input
          value={draft.doi ?? ''}
          onChange={(event) => set({ doi: event.target.value || undefined })}
          className={inputClass}
          placeholder="DOI"
        />
      </div>
      <div className="grid grid-cols-2 gap-2 md:grid-cols-4">
        <input
          value={draft.pmid ?? ''}
          onChange={(event) => set({ pmid: event.target.value || undefined })}
          className={inputClass}
          placeholder="PMID"
        />
        <input
          value={draft.pmcid ?? ''}
          onChange={(event) => set({ pmcid: event.target.value || undefined })}
          className={inputClass}
          placeholder="PMCID"
        />
        <input
          value={draft.arxiv_id ?? ''}
          onChange={(event) => set({ arxiv_id: event.target.value || undefined })}
          className={inputClass}
          placeholder="arXiv"
        />
        <input
          value={draft.openalex_id ?? ''}
          onChange={(event) => set({ openalex_id: event.target.value || undefined })}
          className={inputClass}
          placeholder="OpenAlex ID"
        />
      </div>
      <textarea
        value={draft.abstract_text ?? ''}
        onChange={(event) => set({ abstract_text: event.target.value || undefined })}
        className={`${inputClass} min-h-16 resize-y py-2`}
        placeholder="Abstract"
      />
      <div className="flex gap-2">
        <input
          value={draft.tags?.join(', ') ?? ''}
          onChange={(event) => set({ tags: splitComma(event.target.value) })}
          className={`${inputClass} min-w-0 flex-1`}
          placeholder="Tags"
        />
        <button
          type="button"
          onClick={save}
          disabled={!draft.title.trim() || saving}
          className="h-8 rounded-md bg-[var(--accent)] px-4 text-xs font-medium text-white disabled:opacity-50"
        >
          {saving ? 'Adding...' : 'Add'}
        </button>
      </div>
    </div>
  );
}

function PanelMode({
  active,
  icon,
  label,
  onClick,
}: {
  active: boolean;
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      className={`flex h-8 items-center gap-1 rounded-md px-2 text-[11px] ${active ? 'bg-[var(--bg-sidebar-active)] text-[var(--text-primary)]' : 'text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]'}`}
    >
      {icon} {label}
    </button>
  );
}

function splitComma(value: string): string[] {
  return value
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean);
}

const inputClass =
  'h-8 w-full rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] px-2 text-xs text-[var(--text-primary)] outline-none focus:border-[var(--accent)]';

export default PaperPanel;
