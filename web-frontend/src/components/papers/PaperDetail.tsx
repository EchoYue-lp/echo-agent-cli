import { useState, useEffect } from 'react';
import { papersApi, type Paper } from '../../api/endpoints';
import {
  X,
  Save,
  Tag,
  ExternalLink,
  FileText,
  Calendar,
  Users,
  BookOpen,
  Hash,
  Edit3,
} from 'lucide-react';

interface PaperDetailProps {
  paper: Paper;
  onClose: () => void;
  onUpdated: () => void;
}

export function PaperDetail({ paper, onClose, onUpdated }: PaperDetailProps) {
  const [notes, setNotes] = useState(paper.notes || '');
  const [editingNotes, setEditingNotes] = useState(false);
  const [newTag, setNewTag] = useState('');
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    setNotes(paper.notes || '');
    setEditingNotes(false);
  }, [paper.id, paper.notes]);

  const handleSaveNotes = async () => {
    setSaving(true);
    try {
      await papersApi.updateNotes(paper.id, notes);
      setEditingNotes(false);
      onUpdated();
    } catch (e) {
      console.error('Failed to save notes:', e);
    } finally {
      setSaving(false);
    }
  };

  const handleAddTag = async () => {
    if (!newTag.trim()) return;
    try {
      await papersApi.addTags(paper.id, [newTag.trim()]);
      setNewTag('');
      onUpdated();
    } catch (e) {
      console.error('Failed to add tag:', e);
    }
  };

  const handleDelete = async () => {
    if (!confirm('Delete this paper?')) return;
    try {
      await papersApi.delete(paper.id);
      onClose();
      onUpdated();
    } catch (e) {
      console.error('Failed to delete paper:', e);
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
    <div className="flex flex-col h-full overflow-y-auto">
      {/* Header */}
      <div
        className="flex items-start justify-between p-4 sticky top-0 z-10"
        style={{ background: s.bg, borderBottom: `1px solid ${s.border}` }}
      >
        <div className="flex-1 min-w-0">
          <h2 className="text-sm font-semibold leading-tight" style={{ color: s.text }}>
            {paper.title}
          </h2>
          {paper.authors.length > 0 && (
            <div className="flex items-center gap-1.5 mt-1.5">
              <Users size={11} style={{ color: s.textTer }} />
              <p className="text-xs truncate" style={{ color: s.textSec }}>
                {paper.authors.join(', ')}
              </p>
            </div>
          )}
        </div>
        <div className="flex items-center gap-1 ml-3 flex-shrink-0">
          <button
            onClick={handleDelete}
            className="rounded-md p-1.5 transition-colors"
            style={{ color: 'var(--color-error)' }}
            title="Delete paper"
          >
            <X size={14} />
          </button>
        </div>
      </div>

      <div className="p-4 space-y-4">
        {/* Metadata grid */}
        <div
          className="grid grid-cols-2 gap-3 rounded-lg border p-3"
          style={{ borderColor: s.border, background: s.bgCard }}
        >
          {paper.year && <MetaItem icon={Calendar} label="Year" value={String(paper.year)} s={s} />}
          {paper.venue && <MetaItem icon={BookOpen} label="Venue" value={paper.venue} s={s} />}
          {paper.doi && <MetaItem icon={Hash} label="DOI" value={paper.doi} s={s} />}
          {paper.pmid && <MetaItem icon={Hash} label="PMID" value={paper.pmid} s={s} />}
          {paper.pmcid && <MetaItem icon={Hash} label="PMCID" value={paper.pmcid} s={s} />}
          {paper.arxiv_id && (
            <MetaItem
              icon={ExternalLink}
              label="arXiv"
              value={paper.arxiv_id}
              link={`https://arxiv.org/abs/${paper.arxiv_id}`}
              s={s}
            />
          )}
          {paper.openalex_id && (
            <MetaItem
              icon={ExternalLink}
              label="OpenAlex"
              value={paper.openalex_id}
              link={`https://openalex.org/${paper.openalex_id}`}
              s={s}
            />
          )}
          {paper.zotero_key && (
            <MetaItem icon={Hash} label="Zotero" value={paper.zotero_key} s={s} />
          )}
          {paper.clinical_trial_id && (
            <MetaItem
              icon={ExternalLink}
              label="Clinical trial"
              value={paper.clinical_trial_id}
              link={`https://clinicaltrials.gov/study/${paper.clinical_trial_id}`}
              s={s}
            />
          )}
          {paper.europe_pmc?.full_text_path && (
            <MetaItem
              icon={FileText}
              label="Full text"
              value={paper.europe_pmc.full_text_path}
              s={s}
            />
          )}
          {paper.pdf_path && <MetaItem icon={FileText} label="PDF" value={paper.pdf_path} s={s} />}
          <MetaItem
            icon={Calendar}
            label="Added"
            value={new Date(paper.added_at).toLocaleDateString()}
            s={s}
          />
        </div>

        {/* Abstract */}
        {paper.abstract_text && (
          <div>
            <h3 className="text-xs font-semibold mb-1.5" style={{ color: s.text }}>
              Abstract
            </h3>
            <p
              className="text-xs leading-relaxed rounded-lg border p-3"
              style={{ color: s.textSec, borderColor: s.border, background: s.bgCard }}
            >
              {paper.abstract_text}
            </p>
          </div>
        )}

        {paper.europe_pmc && (
          <div>
            <h3 className="mb-1.5 text-xs font-semibold" style={{ color: s.text }}>
              Europe PMC
            </h3>
            <div className="space-y-2 border-y py-3" style={{ borderColor: s.border }}>
              <div className="text-[11px]" style={{ color: s.textSec }}>
                {paper.europe_pmc.citation_ids.length} citations ·{' '}
                {paper.europe_pmc.reference_ids.length} references ·{' '}
                {paper.europe_pmc.biomedical_entities.length} entities
              </div>
              {paper.europe_pmc.biomedical_entities.length > 0 && (
                <div className="flex flex-wrap gap-1">
                  {paper.europe_pmc.biomedical_entities.slice(0, 20).map((entity) => (
                    <span
                      key={`${entity.semantic_type ?? 'entity'}:${entity.name}`}
                      className="rounded border px-1.5 py-0.5 text-[10px]"
                      style={{ borderColor: s.border, color: s.textSec }}
                    >
                      {entity.name}
                    </span>
                  ))}
                </div>
              )}
            </div>
          </div>
        )}

        {/* Tags */}
        <div>
          <h3
            className="text-xs font-semibold mb-1.5 flex items-center gap-1.5"
            style={{ color: s.text }}
          >
            <Tag size={11} />
            Tags
          </h3>
          <div className="flex flex-wrap gap-1.5">
            {paper.tags.map((tag) => (
              <span
                key={tag}
                className="text-[10px] px-2 py-0.5 rounded-full"
                style={{ background: 'rgba(99,102,241,0.1)', color: 'var(--color-primary)' }}
              >
                {tag}
              </span>
            ))}
            <div className="flex items-center gap-1">
              <input
                type="text"
                value={newTag}
                onChange={(e) => setNewTag(e.target.value)}
                placeholder="Add tag"
                className="rounded-md border px-2 py-0.5 text-[10px] w-20"
                style={{ borderColor: s.border, background: s.bg, color: s.text }}
                onKeyDown={(e) => e.key === 'Enter' && handleAddTag()}
              />
            </div>
          </div>
        </div>

        {/* Notes */}
        <div>
          <div className="flex items-center justify-between mb-1.5">
            <h3
              className="text-xs font-semibold flex items-center gap-1.5"
              style={{ color: s.text }}
            >
              <Edit3 size={11} />
              Notes
            </h3>
            {!editingNotes ? (
              <button
                onClick={() => setEditingNotes(true)}
                className="text-[10px] px-2 py-0.5 rounded-md transition-colors"
                style={{ color: 'var(--color-primary)', background: 'rgba(99,102,241,0.1)' }}
              >
                Edit
              </button>
            ) : (
              <button
                onClick={handleSaveNotes}
                disabled={saving}
                className="flex items-center gap-1 text-[10px] px-2 py-0.5 rounded-md transition-colors"
                style={{ background: 'var(--color-primary)', color: '#fff' }}
              >
                <Save size={10} />
                {saving ? 'Saving...' : 'Save'}
              </button>
            )}
          </div>
          {editingNotes ? (
            <textarea
              value={notes}
              onChange={(e) => setNotes(e.target.value)}
              className="w-full rounded-lg border p-3 text-xs leading-relaxed"
              style={{ borderColor: s.border, background: s.bgCard, color: s.text }}
              rows={8}
              autoFocus
            />
          ) : (
            <div
              className="rounded-lg border p-3 text-xs leading-relaxed min-h-[60px] cursor-text"
              style={{ borderColor: s.border, background: s.bgCard, color: s.textSec }}
              onClick={() => setEditingNotes(true)}
            >
              {paper.notes || <span style={{ color: s.textTer }}>Click to add notes...</span>}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function MetaItem({
  icon: Icon,
  label,
  value,
  link,
  s,
}: {
  icon: typeof Calendar;
  label: string;
  value: string;
  link?: string;
  s: Record<string, string>;
}) {
  return (
    <div>
      <div className="flex items-center gap-1 mb-0.5">
        <Icon size={10} style={{ color: s.textTer }} />
        <span className="text-[10px]" style={{ color: s.textTer }}>
          {label}
        </span>
      </div>
      {link ? (
        <a
          href={link}
          target="_blank"
          rel="noopener noreferrer"
          className="text-xs hover:underline"
          style={{ color: 'var(--color-primary)' }}
        >
          {value}
        </a>
      ) : (
        <p className="text-xs" style={{ color: s.text }}>
          {value}
        </p>
      )}
    </div>
  );
}

export default PaperDetail;
