import { useState, useEffect, useCallback } from 'react';
import { papersApi, type Paper } from '../../api/endpoints';
import { Grid3X3, Download, Sparkles, Plus, Trash2 } from 'lucide-react';

/**
 * ReviewMatrix — paper comparison matrix.
 *
 * Papers are columns, dimensions (Method, Dataset, Key Finding, Limitation, …)
 * are rows.  Each cell is editable.  An "Agent Extract" button lets the agent
 * pre-fill cells, and the whole matrix can be exported to CSV.
 */

interface Dimension {
  id: string;
  label: string;
}

const DEFAULT_DIMENSIONS: Dimension[] = [
  { id: 'method', label: 'Method' },
  { id: 'dataset', label: 'Dataset' },
  { id: 'finding', label: 'Key Finding' },
  { id: 'limitation', label: 'Limitation' },
  { id: 'contribution', label: 'Contribution' },
];

/** matrixData[paperId][dimensionId] = cell text */
type MatrixData = Record<string, Record<string, string>>;

const STORAGE_KEY = 'echo-review-matrix';

function loadMatrix(): MatrixData {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return raw ? JSON.parse(raw) : {};
  } catch {
    return {};
  }
}

function saveMatrix(data: MatrixData) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(data));
}

export function ReviewMatrix() {
  const [papers, setPapers] = useState<Paper[]>([]);
  const [matrix, setMatrix] = useState<MatrixData>(loadMatrix);
  const [loading, setLoading] = useState(true);
  const [editingCell, setEditingCell] = useState<{ paperId: string; dimId: string } | null>(null);
  const [dimensions, setDimensions] = useState<Dimension[]>(DEFAULT_DIMENSIONS);
  const [newDimLabel, setNewDimLabel] = useState('');
  const [showAddDim, setShowAddDim] = useState(false);

  // Persist matrix on change
  useEffect(() => {
    saveMatrix(matrix);
  }, [matrix]);

  // Fetch papers
  useEffect(() => {
    (async () => {
      try {
        const data = await papersApi.list();
        setPapers(data);
      } catch (e) {
        console.error('Failed to fetch papers:', e);
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const getCell = useCallback(
    (paperId: string, dimId: string): string => matrix[paperId]?.[dimId] ?? '',
    [matrix]
  );

  const setCell = (paperId: string, dimId: string, value: string) => {
    setMatrix((prev) => ({
      ...prev,
      [paperId]: { ...(prev[paperId] ?? {}), [dimId]: value },
    }));
  };

  // Add a custom dimension
  const addDimension = () => {
    if (!newDimLabel.trim()) return;
    const id = newDimLabel.trim().toLowerCase().replace(/\s+/g, '_');
    if (dimensions.some((d) => d.id === id)) return;
    setDimensions((prev) => [...prev, { id, label: newDimLabel.trim() }]);
    setNewDimLabel('');
    setShowAddDim(false);
  };

  const removeDimension = (dimId: string) => {
    setDimensions((prev) => prev.filter((d) => d.id !== dimId));
    setMatrix((prev) => {
      const next = { ...prev };
      for (const pid of Object.keys(next)) {
        if (next[pid][dimId] !== undefined) {
          next[pid] = { ...next[pid] };
          delete next[pid][dimId];
        }
      }
      return next;
    });
  };

  // Agent extract — removed: paper analysis should be done through Agent conversation
  const agentExtract = async (paperId: string) => {
    const paper = papers.find((p) => p.id === paperId);
    if (!paper) return;

    // Paper analysis is now handled by the Agent through conversation
    // Users should ask the Agent to analyze papers using tools like arxiv_search
    alert(
      '💡 论文分析请通过 Agent 对话使用研究工具。\n\n例如：\n- "分析这篇论文的方法和数据集"\n- "提取这篇论文的关键发现"\n- "总结这篇论文的贡献和局限性"'
    );
  };

  // Export to CSV
  const exportCsv = () => {
    const header = ['Dimension', ...papers.map((p) => `"${p.title.replace(/"/g, '""')}"`)].join(
      ','
    );
    const rows = dimensions.map((d) => {
      const cells = papers.map((p) => `"${getCell(p.id, d.id).replace(/"/g, '""')}"`);
      return [`"${d.label}"`, ...cells].join(',');
    });
    const csv = [header, ...rows].join('\n');
    const blob = new Blob([csv], { type: 'text/csv' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = 'review_matrix.csv';
    a.click();
    URL.revokeObjectURL(url);
  };

  // Styles
  const s = {
    text: 'var(--text-primary)',
    textSec: 'var(--text-secondary)',
    textTer: 'var(--text-tertiary)',
    border: 'var(--border-primary)',
    bg: 'var(--bg-primary)',
    bgHover: 'var(--bg-hover)',
    bgCard: 'var(--bg-secondary)',
  };

  if (loading) {
    return (
      <div className="p-4">
        <p className="text-xs" style={{ color: s.textTer }}>
          Loading papers…
        </p>
      </div>
    );
  }

  if (papers.length === 0) {
    return (
      <div className="p-6 text-center">
        <Grid3X3 size={24} className="mx-auto mb-2" style={{ color: s.textTer }} />
        <p className="text-xs" style={{ color: s.textTer }}>
          Add papers to use the review matrix
        </p>
      </div>
    );
  }

  return (
    <div className="p-3 space-y-3">
      {/* Header */}
      <div className="flex items-center justify-between flex-wrap gap-2">
        <h3 className="text-sm font-semibold flex items-center gap-1.5" style={{ color: s.text }}>
          <Grid3X3 size={14} />
          Literature Review Matrix
        </h3>
        <div className="flex items-center gap-1.5">
          <button
            onClick={() => setShowAddDim((v) => !v)}
            className="flex items-center gap-1 px-2 py-1 rounded text-[11px] font-medium transition-colors"
            style={{ background: s.bgCard, color: s.textSec, border: `1px solid ${s.border}` }}
            title="Add dimension row"
          >
            <Plus size={11} /> Dimension
          </button>
          <button
            onClick={exportCsv}
            className="flex items-center gap-1 px-2 py-1 rounded text-[11px] font-medium transition-colors"
            style={{ background: s.bgCard, color: s.textSec, border: `1px solid ${s.border}` }}
            title="Export to CSV"
          >
            <Download size={11} /> CSV
          </button>
        </div>
      </div>

      {/* Add dimension inline form */}
      {showAddDim && (
        <div className="flex items-center gap-2">
          <input
            type="text"
            value={newDimLabel}
            onChange={(e) => setNewDimLabel(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && addDimension()}
            placeholder="New dimension label…"
            className="flex-1 rounded border px-2 py-1 text-xs outline-none"
            style={{ borderColor: s.border, background: s.bg, color: s.text }}
            autoFocus
          />
          <button
            onClick={addDimension}
            className="px-2 py-1 rounded text-xs font-medium"
            style={{ background: 'var(--color-primary, #3b82f6)', color: '#fff' }}
          >
            Add
          </button>
        </div>
      )}

      {/* Matrix table — papers as columns, dimensions as rows */}
      <div className="overflow-x-auto rounded-lg border" style={{ borderColor: s.border }}>
        <table className="w-full text-xs">
          <thead>
            <tr style={{ background: s.bgCard }}>
              <th
                className="text-left px-3 py-2 font-medium sticky left-0 z-10"
                style={{
                  color: s.text,
                  background: s.bgCard,
                  borderBottom: `1px solid ${s.border}`,
                  minWidth: 110,
                }}
              >
                Dimension
              </th>
              {papers.map((paper) => (
                <th
                  key={paper.id}
                  className="px-3 py-2 text-left align-top"
                  style={{ borderBottom: `1px solid ${s.border}`, minWidth: 180, maxWidth: 260 }}
                >
                  <p className="font-medium leading-snug" style={{ color: s.text }}>
                    {paper.title}
                  </p>
                  <p className="text-[10px] mt-0.5 truncate" style={{ color: s.textTer }}>
                    {paper.authors[0] ?? 'Unknown'}
                    {paper.authors.length > 1 ? ' et al.' : ''}
                    {paper.year ? ` (${paper.year})` : ''}
                  </p>
                  <button
                    onClick={() => agentExtract(paper.id)}
                    className="mt-1 flex items-center gap-0.5 text-[10px] transition-colors hover:opacity-80"
                    style={{ color: 'var(--color-primary, #3b82f6)' }}
                    title="Auto-extract from abstract"
                  >
                    <Sparkles size={9} /> Agent Extract
                  </button>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {dimensions.map((dim) => (
              <tr key={dim.id} style={{ borderBottom: `1px solid ${s.border}` }}>
                <td
                  className="px-3 py-2 font-medium sticky left-0 z-10 flex items-center gap-1"
                  style={{
                    background: s.bgCard,
                    borderBottom: `1px solid ${s.border}`,
                    color: s.textSec,
                  }}
                >
                  <span className="flex-1">{dim.label}</span>
                  {!DEFAULT_DIMENSIONS.some((d) => d.id === dim.id) && (
                    <button
                      onClick={() => removeDimension(dim.id)}
                      className="p-0.5 rounded transition-colors hover:text-red-500"
                      style={{ color: s.textTer }}
                      title="Remove dimension"
                    >
                      <Trash2 size={9} />
                    </button>
                  )}
                </td>
                {papers.map((paper) => {
                  const isEditing =
                    editingCell?.paperId === paper.id && editingCell?.dimId === dim.id;
                  const value = getCell(paper.id, dim.id);

                  return (
                    <td
                      key={paper.id}
                      className="px-2 py-1.5 align-top"
                      style={{ borderBottom: `1px solid ${s.border}` }}
                    >
                      {isEditing ? (
                        <textarea
                          value={value}
                          onChange={(e) => setCell(paper.id, dim.id, e.target.value)}
                          onBlur={() => setEditingCell(null)}
                          className="w-full p-1.5 text-xs rounded border outline-none resize-y"
                          style={{
                            borderColor: 'var(--border-focus, var(--color-primary, #3b82f6))',
                            background: s.bg,
                            color: s.text,
                            minHeight: 48,
                          }}
                          autoFocus
                          rows={3}
                        />
                      ) : (
                        <div
                          onClick={() => setEditingCell({ paperId: paper.id, dimId: dim.id })}
                          className="min-h-[32px] p-1.5 rounded cursor-text transition-colors whitespace-pre-wrap"
                          style={{
                            color: value ? s.text : s.textTer,
                            background: value ? 'transparent' : s.bgCard,
                          }}
                          title="Click to edit"
                        >
                          {value || <span className="italic">—</span>}
                        </div>
                      )}
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {/* Footer hint */}
      <p className="text-[10px]" style={{ color: s.textTer }}>
        Click any cell to edit. Use "Agent Extract" to auto-fill from paper abstracts. Data is saved
        locally.
      </p>
    </div>
  );
}

export default ReviewMatrix;
