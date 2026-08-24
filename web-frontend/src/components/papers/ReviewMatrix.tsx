import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  evidenceApi,
  papersApi,
  type EvidenceRecord,
  type Paper,
  type ProductDataScope,
} from '../../api/endpoints';
import { Download, Grid3X3, Plus, Trash2 } from 'lucide-react';

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

interface ReviewMatrixProps {
  scope: ProductDataScope;
  reviewId?: string;
  sourceIds?: string[];
}

export function ReviewMatrix({ scope, reviewId, sourceIds }: ReviewMatrixProps) {
  const [papers, setPapers] = useState<Paper[]>([]);
  const [evidence, setEvidence] = useState<EvidenceRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [savingCell, setSavingCell] = useState<string | null>(null);
  const [newDimLabel, setNewDimLabel] = useState('');
  const [showAddDim, setShowAddDim] = useState(false);
  const [customDimensions, setCustomDimensions] = useState<Dimension[]>([]);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const [allPapers, records] = await Promise.all([
        papersApi.list(scope),
        evidenceApi.list(scope, reviewId ? { reviewId } : undefined),
      ]);
      const selected = sourceIds?.length
        ? allPapers.filter((paper) => sourceIds.includes(paper.id))
        : allPapers;
      setPapers(selected);
      setEvidence(
        records.filter((record) =>
          reviewId ? record.review_id === reviewId : record.review_id == null
        )
      );
      setError(null);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setLoading(false);
    }
  }, [reviewId, scope, sourceIds]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const dimensions = useMemo(() => {
    const known = new Map(DEFAULT_DIMENSIONS.map((dimension) => [dimension.id, dimension]));
    for (const dimension of customDimensions) known.set(dimension.id, dimension);
    for (const record of evidence) {
      if (!known.has(record.dimension)) {
        known.set(record.dimension, {
          id: record.dimension,
          label: record.dimension.replaceAll('_', ' '),
        });
      }
    }
    return [...known.values()];
  }, [customDimensions, evidence]);

  const evidenceFor = useCallback(
    (sourceId: string, dimension: string) =>
      evidence.find((record) => record.source_id === sourceId && record.dimension === dimension),
    [evidence]
  );

  const saveCell = async (paperId: string, dimension: string, claim: string) => {
    const existing = evidenceFor(paperId, dimension);
    const cellKey = `${paperId}:${dimension}`;
    setSavingCell(cellKey);
    try {
      const saved = await evidenceApi.upsert(scope, {
        id: existing?.id,
        source_id: paperId,
        review_id: reviewId,
        dimension,
        claim,
        tags: existing?.tags ?? [],
        excerpt: existing?.excerpt,
        locator: existing?.locator,
        evidence_type: existing?.evidence_type,
        population: existing?.population,
        intervention: existing?.intervention,
        comparator: existing?.comparator,
        outcome: existing?.outcome,
        effect: existing?.effect,
        limitations: existing?.limitations,
        certainty: existing?.certainty,
      });
      setEvidence((current) => [...current.filter((record) => record.id !== saved.id), saved]);
      setError(null);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setSavingCell(null);
    }
  };

  const addDimension = () => {
    const label = newDimLabel.trim();
    if (!label) return;
    const id = label
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '_')
      .replace(/^_+|_+$/g, '');
    if (!id || dimensions.some((dimension) => dimension.id === id)) return;
    setCustomDimensions((current) => [...current, { id, label }]);
    setNewDimLabel('');
    setShowAddDim(false);
  };

  const removeDimension = async (dimension: string) => {
    const records = evidence.filter((record) => record.dimension === dimension);
    try {
      await Promise.all(records.map((record) => evidenceApi.delete(scope, record.id)));
      setEvidence((current) => current.filter((record) => record.dimension !== dimension));
      setCustomDimensions((current) => current.filter((item) => item.id !== dimension));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  const exportCsv = () => {
    const quote = (value: string) => `"${value.replaceAll('"', '""')}"`;
    const header = ['Dimension', ...papers.map((paper) => paper.title)].map(quote).join(',');
    const rows = dimensions.map((dimension) =>
      [dimension.label, ...papers.map((paper) => evidenceFor(paper.id, dimension.id)?.claim ?? '')]
        .map(quote)
        .join(',')
    );
    const blob = new Blob([[header, ...rows].join('\n')], { type: 'text/csv;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = reviewId ? `${reviewId}-evidence-matrix.csv` : 'evidence-matrix.csv';
    link.click();
    URL.revokeObjectURL(url);
  };

  if (loading) {
    return <div className="p-4 text-xs text-[var(--text-tertiary)]">Loading evidence...</div>;
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex flex-wrap items-center gap-2 border-b border-[var(--border-primary)] px-3 py-2">
        <div className="flex min-w-0 flex-1 items-center gap-1.5 text-xs font-semibold text-[var(--text-primary)]">
          <Grid3X3 size={13} />
          Evidence matrix
        </div>
        <button
          type="button"
          onClick={() => setShowAddDim((visible) => !visible)}
          className="flex h-7 items-center gap-1 rounded-md border border-[var(--border-primary)] px-2 text-[11px] text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
          title="Add extraction dimension"
        >
          <Plus size={11} /> Dimension
        </button>
        <button
          type="button"
          onClick={exportCsv}
          className="flex h-7 items-center gap-1 rounded-md border border-[var(--border-primary)] px-2 text-[11px] text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
          title="Export evidence matrix"
        >
          <Download size={11} /> CSV
        </button>
      </div>

      {showAddDim && (
        <div className="flex gap-2 border-b border-[var(--border-primary)] p-2">
          <input
            value={newDimLabel}
            onChange={(event) => setNewDimLabel(event.target.value)}
            onKeyDown={(event) => event.key === 'Enter' && addDimension()}
            className="min-w-0 flex-1 rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] px-2 text-xs text-[var(--text-primary)] outline-none"
            placeholder="Dimension name"
          />
          <button
            type="button"
            onClick={addDimension}
            className="h-7 rounded-md bg-[var(--accent)] px-3 text-xs font-medium text-white"
          >
            Add
          </button>
        </div>
      )}

      {error && (
        <div className="border-b border-[var(--border-primary)] bg-red-500/10 px-3 py-2 text-xs text-red-500">
          {error}
        </div>
      )}

      {papers.length === 0 ? (
        <div className="flex flex-1 items-center justify-center p-6 text-center text-xs text-[var(--text-tertiary)]">
          Add sources to begin evidence extraction.
        </div>
      ) : (
        <div className="min-h-0 flex-1 overflow-auto">
          <table className="min-w-full border-collapse text-xs">
            <thead className="sticky top-0 z-20 bg-[var(--bg-secondary)]">
              <tr>
                <th className="sticky left-0 z-30 min-w-28 border-b border-r border-[var(--border-primary)] bg-[var(--bg-secondary)] px-3 py-2 text-left font-medium text-[var(--text-secondary)]">
                  Dimension
                </th>
                {papers.map((paper) => (
                  <th
                    key={paper.id}
                    className="min-w-48 max-w-64 border-b border-r border-[var(--border-primary)] px-3 py-2 text-left align-top"
                  >
                    <div className="line-clamp-2 font-medium text-[var(--text-primary)]">
                      {paper.title}
                    </div>
                    <div className="mt-1 truncate text-[10px] font-normal text-[var(--text-tertiary)]">
                      {paper.authors[0] ?? 'Unknown'} {paper.year ? `(${paper.year})` : ''}
                    </div>
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {dimensions.map((dimension) => (
                <tr key={dimension.id}>
                  <td className="sticky left-0 z-10 border-b border-r border-[var(--border-primary)] bg-[var(--bg-secondary)] px-3 py-2 align-top text-[var(--text-secondary)]">
                    <div className="flex items-center gap-1">
                      <span className="min-w-0 flex-1">{dimension.label}</span>
                      {!DEFAULT_DIMENSIONS.some((item) => item.id === dimension.id) && (
                        <button
                          type="button"
                          onClick={() => void removeDimension(dimension.id)}
                          className="flex h-5 w-5 items-center justify-center rounded text-[var(--text-tertiary)] hover:bg-red-500/10 hover:text-red-500"
                          title="Delete dimension evidence"
                        >
                          <Trash2 size={10} />
                        </button>
                      )}
                    </div>
                  </td>
                  {papers.map((paper) => {
                    const record = evidenceFor(paper.id, dimension.id);
                    const cellKey = `${paper.id}:${dimension.id}`;
                    return (
                      <td
                        key={paper.id}
                        className="border-b border-r border-[var(--border-primary)] p-1 align-top"
                      >
                        <textarea
                          key={`${record?.id ?? cellKey}:${record?.updated_at ?? ''}`}
                          defaultValue={record?.claim ?? ''}
                          onBlur={(event) => {
                            if (event.target.value !== (record?.claim ?? '')) {
                              void saveCell(paper.id, dimension.id, event.target.value);
                            }
                          }}
                          className="h-24 w-full resize-none rounded border border-transparent bg-transparent p-2 text-xs leading-relaxed text-[var(--text-primary)] outline-none hover:border-[var(--border-primary)] focus:border-[var(--accent)] focus:bg-[var(--bg-input)]"
                          placeholder="Extract evidence..."
                        />
                        {savingCell === cellKey && (
                          <div className="px-2 pb-1 text-[10px] text-[var(--text-tertiary)]">
                            Saving...
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
      )}
    </div>
  );
}

export default ReviewMatrix;
