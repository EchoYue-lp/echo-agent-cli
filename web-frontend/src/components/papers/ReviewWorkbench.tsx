import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  papersApi,
  systematicReviewsApi,
  type GradeConcern,
  type Paper,
  type PrismaFlow,
  type ReviewDomain,
  type RiskJudgment,
  type ScreeningDecisionValue,
  type ScreeningStage,
  type SystematicReviewDocument,
  type SystematicReviewRecord,
  type SystematicReviewSummary,
} from '../../api/endpoints';
import {
  Activity,
  ClipboardCheck,
  FileSearch,
  Plus,
  Save,
  ShieldCheck,
  Trash2,
} from 'lucide-react';
import { ReviewMatrix } from './ReviewMatrix';

type Section = 'protocol' | 'screening' | 'evidence' | 'quality' | 'prisma';

const EMPTY_GRADE_DOMAIN = { concern: 'not_serious' as GradeConcern, explanation: '' };

export function computePrismaFlow(record: SystematicReviewRecord): PrismaFlow {
  const title = record.screening.filter((item) => item.stage === 'title_abstract');
  const fullText = record.screening.filter((item) => item.stage === 'full_text');
  return {
    records_identified: record.source_ids.length + record.prisma.additional_identified,
    duplicates_removed: record.prisma.duplicates_removed,
    records_screened: title.length,
    records_excluded: title.filter((item) => item.decision === 'exclude').length,
    reports_sought: title.filter((item) => item.decision === 'include').length,
    reports_not_retrieved: record.prisma.reports_not_retrieved,
    reports_assessed: fullText.length,
    reports_excluded: fullText.filter((item) => item.decision === 'exclude').length,
    studies_included: fullText.filter((item) => item.decision === 'include').length,
  };
}

export function ReviewWorkbench() {
  const [summaries, setSummaries] = useState<SystematicReviewSummary[]>([]);
  const [sources, setSources] = useState<Paper[]>([]);
  const [document, setDocument] = useState<SystematicReviewDocument | null>(null);
  const [section, setSection] = useState<Section>('protocol');
  const [showCreate, setShowCreate] = useState(false);
  const [newTitle, setNewTitle] = useState('');
  const [newQuestion, setNewQuestion] = useState('');
  const [newDomain, setNewDomain] = useState<ReviewDomain>('academic');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refreshIndex = useCallback(async () => {
    try {
      const [reviewList, paperList] = await Promise.all([
        systematicReviewsApi.list(),
        papersApi.list(),
      ]);
      setSummaries(reviewList);
      setSources(paperList);
      setError(null);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }, []);

  useEffect(() => {
    void refreshIndex();
  }, [refreshIndex]);

  const loadReview = async (reviewId: string) => {
    try {
      setDocument(await systematicReviewsApi.get(reviewId));
      setSection('protocol');
      setError(null);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  const createReview = async () => {
    if (!newTitle.trim()) return;
    try {
      const created = await systematicReviewsApi.create({
        title: newTitle.trim(),
        question: newQuestion.trim(),
        domain: newDomain,
      });
      setDocument(created);
      setShowCreate(false);
      setNewTitle('');
      setNewQuestion('');
      await refreshIndex();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  const updateRecord = (update: (record: SystematicReviewRecord) => SystematicReviewRecord) => {
    setDocument((current) => (current ? { ...current, record: update(current.record) } : current));
  };

  const saveReview = async () => {
    if (!document) return;
    setSaving(true);
    try {
      const saved = await systematicReviewsApi.save(document);
      setDocument(saved);
      await refreshIndex();
      setError(null);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setSaving(false);
    }
  };

  const deleteReview = async () => {
    if (!document || !confirm(`Delete review “${document.record.title}”?`)) return;
    try {
      await systematicReviewsApi.delete(document.record.id);
      setDocument(null);
      await refreshIndex();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  return (
    <div className="flex h-full min-h-0">
      <aside className="flex w-44 shrink-0 flex-col border-r border-[var(--border-primary)] bg-[var(--bg-secondary)]">
        <div className="flex items-center justify-between border-b border-[var(--border-primary)] px-2 py-2">
          <span className="text-xs font-semibold text-[var(--text-primary)]">Reviews</span>
          <button
            type="button"
            onClick={() => setShowCreate((visible) => !visible)}
            className="flex h-6 w-6 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            title="New review"
          >
            <Plus size={12} />
          </button>
        </div>
        {showCreate && (
          <div className="space-y-2 border-b border-[var(--border-primary)] p-2">
            <input
              value={newTitle}
              onChange={(event) => setNewTitle(event.target.value)}
              className="h-7 w-full rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] px-2 text-[11px] text-[var(--text-primary)] outline-none"
              placeholder="Review title"
            />
            <textarea
              value={newQuestion}
              onChange={(event) => setNewQuestion(event.target.value)}
              className="h-14 w-full resize-none rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] p-2 text-[11px] text-[var(--text-primary)] outline-none"
              placeholder="Research question"
            />
            <select
              value={newDomain}
              onChange={(event) => setNewDomain(event.target.value as ReviewDomain)}
              className="h-7 w-full rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] px-2 text-[11px] text-[var(--text-primary)]"
            >
              <option value="academic">Academic</option>
              <option value="medical">Medical</option>
            </select>
            <button
              type="button"
              onClick={() => void createReview()}
              className="h-7 w-full rounded-md bg-[var(--accent)] text-[11px] font-medium text-white"
            >
              Create
            </button>
          </div>
        )}
        <div className="min-h-0 flex-1 overflow-y-auto">
          {summaries.map((summary) => (
            <button
              key={summary.id}
              type="button"
              onClick={() => void loadReview(summary.id)}
              className={`w-full border-b border-[var(--border-primary)] px-3 py-2 text-left hover:bg-[var(--bg-hover)] ${document?.record.id === summary.id ? 'bg-[var(--bg-sidebar-active)]' : ''}`}
            >
              <div className="line-clamp-2 text-[11px] font-medium text-[var(--text-primary)]">
                {summary.title}
              </div>
              <div className="mt-1 text-[10px] text-[var(--text-tertiary)]">
                {summary.domain === 'medical' ? 'Medical' : 'Academic'} · {summary.source_count}{' '}
                sources
              </div>
            </button>
          ))}
        </div>
      </aside>

      <main className="flex min-w-0 flex-1 flex-col">
        {error && (
          <div className="border-b border-[var(--border-primary)] bg-red-500/10 px-3 py-2 text-xs text-red-500">
            {error}
          </div>
        )}
        {!document ? (
          <div className="flex flex-1 items-center justify-center p-6 text-center text-xs text-[var(--text-tertiary)]">
            Select or create a systematic review.
          </div>
        ) : (
          <ReviewEditor
            document={document}
            sources={sources}
            section={section}
            setSection={setSection}
            updateRecord={updateRecord}
            save={() => void saveReview()}
            remove={() => void deleteReview()}
            saving={saving}
          />
        )}
      </main>
    </div>
  );
}

function ReviewEditor({
  document,
  sources,
  section,
  setSection,
  updateRecord,
  save,
  remove,
  saving,
}: {
  document: SystematicReviewDocument;
  sources: Paper[];
  section: Section;
  setSection: (section: Section) => void;
  updateRecord: (update: (record: SystematicReviewRecord) => SystematicReviewRecord) => void;
  save: () => void;
  remove: () => void;
  saving: boolean;
}) {
  const record = document.record;
  const flow = useMemo(() => computePrismaFlow(record), [record]);
  const tabs: Array<{ id: Section; label: string; icon: typeof FileSearch }> = [
    { id: 'protocol', label: 'Protocol', icon: FileSearch },
    { id: 'screening', label: 'Screen', icon: ClipboardCheck },
    { id: 'evidence', label: 'Evidence', icon: Activity },
    { id: 'quality', label: 'Quality', icon: ShieldCheck },
    { id: 'prisma', label: 'PRISMA', icon: Activity },
  ];

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex min-h-10 items-center gap-2 border-b border-[var(--border-primary)] px-2">
        <div className="min-w-0 flex-1 truncate px-1 text-xs font-semibold text-[var(--text-primary)]">
          {record.title}
        </div>
        <button
          type="button"
          onClick={save}
          disabled={saving}
          className="flex h-7 items-center gap-1 rounded-md bg-[var(--accent)] px-2.5 text-[11px] font-medium text-white disabled:opacity-50"
        >
          <Save size={11} /> {saving ? 'Saving...' : 'Save'}
        </button>
        <button
          type="button"
          onClick={remove}
          className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-red-500/10 hover:text-red-500"
          title="Delete review"
        >
          <Trash2 size={12} />
        </button>
      </header>
      <div className="flex h-9 shrink-0 items-center border-b border-[var(--border-primary)] px-1">
        {tabs.map(({ id, label, icon: Icon }) => (
          <button
            key={id}
            type="button"
            onClick={() => setSection(id)}
            className={`flex h-9 items-center gap-1 px-2 text-[11px] ${section === id ? 'border-b-2 border-[var(--accent)] text-[var(--text-primary)]' : 'text-[var(--text-tertiary)] hover:text-[var(--text-primary)]'}`}
          >
            <Icon size={11} /> {label}
          </button>
        ))}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {section === 'protocol' ? (
          <ProtocolEditor record={record} updateRecord={updateRecord} />
        ) : section === 'screening' ? (
          <ScreeningEditor record={record} sources={sources} updateRecord={updateRecord} />
        ) : section === 'evidence' ? (
          <ReviewMatrix reviewId={record.id} sourceIds={record.source_ids} />
        ) : section === 'quality' ? (
          <QualityEditor record={record} sources={sources} updateRecord={updateRecord} />
        ) : (
          <PrismaEditor flow={flow} record={record} updateRecord={updateRecord} />
        )}
      </div>
    </div>
  );
}

function ProtocolEditor({
  record,
  updateRecord,
}: {
  record: SystematicReviewRecord;
  updateRecord: (update: (record: SystematicReviewRecord) => SystematicReviewRecord) => void;
}) {
  const setProtocol = (patch: Partial<SystematicReviewRecord['protocol']>) =>
    updateRecord((current) => ({
      ...current,
      protocol: { ...current.protocol, ...patch },
    }));
  return (
    <div className="space-y-4 p-4">
      <Field label="Title">
        <input
          value={record.title}
          onChange={(event) =>
            updateRecord((current) => ({ ...current, title: event.target.value }))
          }
          className={inputClass}
        />
      </Field>
      <Field label="Review status">
        <select
          value={record.status}
          onChange={(event) =>
            updateRecord((current) => ({ ...current, status: event.target.value }))
          }
          className={inputClass}
        >
          <option value="protocol">Protocol</option>
          <option value="screening">Screening</option>
          <option value="extraction">Extraction</option>
          <option value="synthesis">Synthesis</option>
          <option value="complete">Complete</option>
        </select>
      </Field>
      <Field label="Objective">
        <textarea
          value={record.protocol.objective}
          onChange={(event) => setProtocol({ objective: event.target.value })}
          className={`${inputClass} min-h-20 resize-y py-2`}
        />
      </Field>
      <Field label="Review question">
        <textarea
          value={record.protocol.question}
          onChange={(event) => setProtocol({ question: event.target.value })}
          className={`${inputClass} min-h-20 resize-y py-2`}
        />
      </Field>
      <div className="grid gap-3 md:grid-cols-2">
        <LinesField
          label="Inclusion criteria"
          values={record.protocol.eligibility.inclusion}
          onChange={(inclusion) =>
            setProtocol({
              eligibility: { ...record.protocol.eligibility, inclusion },
            })
          }
        />
        <LinesField
          label="Exclusion criteria"
          values={record.protocol.eligibility.exclusion}
          onChange={(exclusion) =>
            setProtocol({
              eligibility: { ...record.protocol.eligibility, exclusion },
            })
          }
        />
      </div>
      <LinesField
        label="Search databases"
        values={record.protocol.databases}
        onChange={(databases) => setProtocol({ databases })}
      />
      <div>
        <div className="mb-2 flex items-center justify-between">
          <span className="text-[11px] text-[var(--text-tertiary)]">Search log</span>
          <button
            type="button"
            onClick={() =>
              setProtocol({
                search_strategies: [
                  ...record.protocol.search_strategies,
                  { database: '', query: '' },
                ],
              })
            }
            className="flex h-7 items-center gap-1 rounded-md border border-[var(--border-primary)] px-2 text-[11px] text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
          >
            <Plus size={11} /> Search
          </button>
        </div>
        <div className="space-y-2">
          {record.protocol.search_strategies.map((strategy, index) => (
            <div
              key={`${index}:${strategy.database}`}
              className="grid gap-2 border-b border-[var(--border-primary)] pb-2 md:grid-cols-[130px_minmax(220px,1fr)_170px_90px_28px]"
            >
              <input
                value={strategy.database}
                onChange={(event) =>
                  setProtocol({
                    search_strategies: replaceAt(record.protocol.search_strategies, index, {
                      ...strategy,
                      database: event.target.value,
                    }),
                  })
                }
                className={inputClass}
                placeholder="Database"
              />
              <input
                value={strategy.query}
                onChange={(event) =>
                  setProtocol({
                    search_strategies: replaceAt(record.protocol.search_strategies, index, {
                      ...strategy,
                      query: event.target.value,
                    }),
                  })
                }
                className={inputClass}
                placeholder="Exact query"
              />
              <input
                value={strategy.searched_at ?? ''}
                onChange={(event) =>
                  setProtocol({
                    search_strategies: replaceAt(record.protocol.search_strategies, index, {
                      ...strategy,
                      searched_at: event.target.value || undefined,
                    }),
                  })
                }
                className={inputClass}
                placeholder="2026-07-18T10:00:00Z"
              />
              <input
                type="number"
                min="0"
                value={strategy.result_count ?? ''}
                onChange={(event) =>
                  setProtocol({
                    search_strategies: replaceAt(record.protocol.search_strategies, index, {
                      ...strategy,
                      result_count: event.target.value ? Number(event.target.value) : undefined,
                    }),
                  })
                }
                className={inputClass}
                placeholder="Hits"
              />
              <button
                type="button"
                onClick={() =>
                  setProtocol({
                    search_strategies: record.protocol.search_strategies.filter(
                      (_, position) => position !== index
                    ),
                  })
                }
                className="flex h-8 w-7 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-red-500/10 hover:text-red-500"
                title="Remove search log entry"
              >
                <Trash2 size={11} />
              </button>
            </div>
          ))}
        </div>
      </div>
      <div className="grid gap-3 md:grid-cols-2">
        <Field label="Registration">
          <input
            value={record.protocol.registration ?? ''}
            onChange={(event) => setProtocol({ registration: event.target.value || undefined })}
            className={inputClass}
            placeholder="PROSPERO, OSF, DOI..."
          />
        </Field>
        <Field label="Date range">
          <input
            value={record.protocol.date_range ?? ''}
            onChange={(event) => setProtocol({ date_range: event.target.value || undefined })}
            className={inputClass}
            placeholder="2000-01-01 to 2026-12-31"
          />
        </Field>
      </div>
      {record.domain === 'medical' && record.protocol.pico && (
        <div className="border-t border-[var(--border-primary)] pt-4">
          <div className="mb-3 text-xs font-semibold text-[var(--text-primary)]">PICO</div>
          <div className="grid gap-3 md:grid-cols-2">
            {(['population', 'intervention', 'comparator'] as const).map((field) => (
              <Field key={field} label={field[0]?.toUpperCase() + field.slice(1)}>
                <input
                  value={record.protocol.pico?.[field] ?? ''}
                  onChange={(event) =>
                    setProtocol({
                      pico: { ...record.protocol.pico!, [field]: event.target.value },
                    })
                  }
                  className={inputClass}
                />
              </Field>
            ))}
            <LinesField
              label="Outcomes"
              values={record.protocol.pico.outcomes}
              onChange={(outcomes) => setProtocol({ pico: { ...record.protocol.pico!, outcomes } })}
            />
          </div>
        </div>
      )}
    </div>
  );
}

function ScreeningEditor({
  record,
  sources,
  updateRecord,
}: {
  record: SystematicReviewRecord;
  sources: Paper[];
  updateRecord: (update: (record: SystematicReviewRecord) => SystematicReviewRecord) => void;
}) {
  const selected = new Set(record.source_ids);
  const toggleSource = (sourceId: string) =>
    updateRecord((current) => ({
      ...current,
      source_ids: current.source_ids.includes(sourceId)
        ? current.source_ids.filter((id) => id !== sourceId)
        : [...current.source_ids, sourceId],
      screening: current.source_ids.includes(sourceId)
        ? current.screening.filter((item) => item.source_id !== sourceId)
        : current.screening,
    }));
  const setDecision = (
    sourceId: string,
    stage: ScreeningStage,
    decision: ScreeningDecisionValue,
    reason?: string
  ) =>
    updateRecord((current) => ({
      ...current,
      screening: [
        ...current.screening.filter(
          (item) => !(item.source_id === sourceId && item.stage === stage)
        ),
        {
          source_id: sourceId,
          stage,
          decision,
          reason,
          decided_at: new Date().toISOString(),
        },
      ],
    }));

  return (
    <div className="min-w-[680px]">
      <div className="grid grid-cols-[36px_minmax(180px,1fr)_130px_130px_minmax(160px,1fr)] border-b border-[var(--border-primary)] bg-[var(--bg-secondary)] px-2 py-2 text-[10px] font-medium text-[var(--text-tertiary)]">
        <span />
        <span>Source</span>
        <span>Title/abstract</span>
        <span>Full text</span>
        <span>Exclusion reason</span>
      </div>
      {sources.map((source) => {
        const titleDecision = record.screening.find(
          (item) => item.source_id === source.id && item.stage === 'title_abstract'
        );
        const fullDecision = record.screening.find(
          (item) => item.source_id === source.id && item.stage === 'full_text'
        );
        const active = selected.has(source.id);
        return (
          <div
            key={source.id}
            className="grid grid-cols-[36px_minmax(180px,1fr)_130px_130px_minmax(160px,1fr)] items-center border-b border-[var(--border-primary)] px-2 py-2 text-xs"
          >
            <input
              type="checkbox"
              checked={active}
              onChange={() => toggleSource(source.id)}
              className="h-3.5 w-3.5"
            />
            <div className="min-w-0 pr-3">
              <div className="truncate font-medium text-[var(--text-primary)]">{source.title}</div>
              <div className="truncate text-[10px] text-[var(--text-tertiary)]">
                {source.authors.join(', ')}
              </div>
            </div>
            <DecisionSelect
              disabled={!active}
              value={titleDecision?.decision ?? 'pending'}
              onChange={(decision) =>
                setDecision(source.id, 'title_abstract', decision, titleDecision?.reason)
              }
            />
            <DecisionSelect
              disabled={!active}
              value={fullDecision?.decision ?? 'pending'}
              onChange={(decision) =>
                setDecision(source.id, 'full_text', decision, fullDecision?.reason)
              }
            />
            <input
              disabled={!active}
              value={fullDecision?.reason ?? titleDecision?.reason ?? ''}
              onChange={(event) => {
                const target = fullDecision ? 'full_text' : 'title_abstract';
                const decision = fullDecision?.decision ?? titleDecision?.decision ?? 'pending';
                setDecision(source.id, target, decision, event.target.value || undefined);
              }}
              className={inputClass}
              placeholder="Required when excluded"
            />
          </div>
        );
      })}
    </div>
  );
}

function QualityEditor({
  record,
  sources,
  updateRecord,
}: {
  record: SystematicReviewRecord;
  sources: Paper[];
  updateRecord: (update: (record: SystematicReviewRecord) => SystematicReviewRecord) => void;
}) {
  const selectedSources = sources.filter((source) => record.source_ids.includes(source.id));
  const updateRisk = (sourceId: string, field: 'overall' | 'rationale' | 'tool', value: string) =>
    updateRecord((current) => {
      const existing = current.risk_of_bias.find((item) => item.source_id === sourceId);
      const tool =
        field === 'tool' ? (value as 'rob2' | 'robins_i' | 'custom') : (existing?.tool ?? 'rob2');
      const assessment = {
        id: existing?.id ?? crypto.randomUUID(),
        source_id: sourceId,
        result_id: existing?.result_id,
        tool,
        domains:
          field === 'tool' || !existing?.domains.length
            ? defaultRiskDomains(tool)
            : existing.domains,
        overall: existing?.overall ?? ('low' as RiskJudgment),
        rationale: existing?.rationale ?? '',
        assessed_at: new Date().toISOString(),
        [field]: value,
      };
      return {
        ...current,
        risk_of_bias: [
          ...current.risk_of_bias.filter((item) => item.source_id !== sourceId),
          assessment,
        ],
      };
    });
  const updateRiskDomain = (
    sourceId: string,
    domainName: string,
    patch: { judgment?: RiskJudgment; rationale?: string }
  ) =>
    updateRecord((current) => {
      const existing = current.risk_of_bias.find((item) => item.source_id === sourceId);
      const base = existing ?? {
        id: crypto.randomUUID(),
        source_id: sourceId,
        tool: 'rob2' as const,
        domains: defaultRiskDomains('rob2'),
        overall: 'low' as RiskJudgment,
        rationale: '',
        assessed_at: new Date().toISOString(),
      };
      const assessment = {
        ...base,
        domains: base.domains.map((domain) =>
          domain.domain === domainName ? { ...domain, ...patch } : domain
        ),
        assessed_at: new Date().toISOString(),
      };
      return {
        ...current,
        risk_of_bias: [
          ...current.risk_of_bias.filter((item) => item.source_id !== sourceId),
          assessment,
        ],
      };
    });
  const addGrade = () =>
    updateRecord((current) => ({
      ...current,
      grade: [
        ...current.grade,
        {
          id: crypto.randomUUID(),
          outcome: 'Outcome',
          certainty: 'moderate',
          risk_of_bias: { ...EMPTY_GRADE_DOMAIN },
          inconsistency: { ...EMPTY_GRADE_DOMAIN },
          indirectness: { ...EMPTY_GRADE_DOMAIN },
          imprecision: { ...EMPTY_GRADE_DOMAIN },
          publication_bias: { ...EMPTY_GRADE_DOMAIN },
        },
      ],
    }));

  return (
    <div className="space-y-6 p-4">
      <section>
        <div className="mb-2 text-xs font-semibold text-[var(--text-primary)]">
          Risk of bias {record.domain === 'medical' ? '(RoB 2 / ROBINS-I)' : ''}
        </div>
        <div className="overflow-x-auto border-y border-[var(--border-primary)]">
          {selectedSources.map((source) => {
            const risk = record.risk_of_bias.find((item) => item.source_id === source.id);
            const domains = risk?.domains.length
              ? risk.domains
              : defaultRiskDomains(risk?.tool ?? 'rob2');
            return (
              <div key={source.id} className="border-b border-[var(--border-primary)] py-2">
                <div className="grid min-w-[640px] grid-cols-[minmax(180px,1fr)_110px_130px_minmax(200px,1fr)] items-center gap-2">
                  <div className="truncate text-xs text-[var(--text-primary)]">{source.title}</div>
                  <select
                    value={risk?.tool ?? 'rob2'}
                    onChange={(event) => updateRisk(source.id, 'tool', event.target.value)}
                    className={inputClass}
                  >
                    <option value="rob2">RoB 2</option>
                    <option value="robins_i">ROBINS-I</option>
                    <option value="custom">Custom</option>
                  </select>
                  <select
                    value={risk?.overall ?? 'low'}
                    onChange={(event) => updateRisk(source.id, 'overall', event.target.value)}
                    className={inputClass}
                  >
                    <option value="low">Low</option>
                    <option value="some_concerns">Some concerns</option>
                    <option value="high">High</option>
                  </select>
                  <input
                    value={risk?.rationale ?? ''}
                    onChange={(event) => updateRisk(source.id, 'rationale', event.target.value)}
                    className={inputClass}
                    placeholder="Overall judgment rationale"
                  />
                </div>
                <div className="ml-4 mt-2 space-y-1 border-l border-[var(--border-primary)] pl-3">
                  {domains.map((domain) => (
                    <div
                      key={domain.domain}
                      className="grid min-w-[620px] grid-cols-[minmax(220px,1fr)_130px_minmax(240px,1fr)] items-center gap-2"
                    >
                      <span className="text-[10px] text-[var(--text-tertiary)]">
                        {domain.domain}
                      </span>
                      <select
                        value={domain.judgment}
                        onChange={(event) =>
                          updateRiskDomain(source.id, domain.domain, {
                            judgment: event.target.value as RiskJudgment,
                          })
                        }
                        className={inputClass}
                      >
                        <option value="low">Low</option>
                        <option value="some_concerns">Some concerns</option>
                        <option value="high">High</option>
                      </select>
                      <input
                        value={domain.rationale}
                        onChange={(event) =>
                          updateRiskDomain(source.id, domain.domain, {
                            rationale: event.target.value,
                          })
                        }
                        className={inputClass}
                        placeholder="Domain rationale"
                      />
                    </div>
                  ))}
                </div>
              </div>
            );
          })}
        </div>
      </section>

      <section>
        <div className="mb-2 flex items-center justify-between">
          <div className="text-xs font-semibold text-[var(--text-primary)]">GRADE outcomes</div>
          <button
            type="button"
            onClick={addGrade}
            className="flex h-7 items-center gap-1 rounded-md border border-[var(--border-primary)] px-2 text-[11px] text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
          >
            <Plus size={11} /> Outcome
          </button>
        </div>
        <div className="space-y-3">
          {record.grade.map((grade) => (
            <div key={grade.id} className="border-y border-[var(--border-primary)] py-3">
              <div className="grid gap-2 md:grid-cols-3">
                <input
                  value={grade.outcome}
                  onChange={(event) =>
                    updateGrade(updateRecord, grade.id, { outcome: event.target.value })
                  }
                  className={inputClass}
                  placeholder="Outcome"
                />
                <input
                  value={grade.relative_effect ?? ''}
                  onChange={(event) =>
                    updateGrade(updateRecord, grade.id, {
                      relative_effect: event.target.value || undefined,
                    })
                  }
                  className={inputClass}
                  placeholder="Relative effect"
                />
                <input
                  value={grade.absolute_effect ?? ''}
                  onChange={(event) =>
                    updateGrade(updateRecord, grade.id, {
                      absolute_effect: event.target.value || undefined,
                    })
                  }
                  className={inputClass}
                  placeholder="Absolute effect"
                />
              </div>
              <div className="mt-2 grid gap-2 md:grid-cols-3">
                <select
                  value={grade.certainty}
                  onChange={(event) =>
                    updateGrade(updateRecord, grade.id, {
                      certainty: event.target.value as typeof grade.certainty,
                    })
                  }
                  className={inputClass}
                >
                  <option value="high">High</option>
                  <option value="moderate">Moderate</option>
                  <option value="low">Low</option>
                  <option value="very_low">Very low</option>
                </select>
                <input
                  type="number"
                  min="0"
                  value={grade.studies ?? ''}
                  onChange={(event) =>
                    updateGrade(updateRecord, grade.id, {
                      studies: event.target.value ? Number(event.target.value) : undefined,
                    })
                  }
                  className={inputClass}
                  placeholder="Studies"
                />
                <input
                  type="number"
                  min="0"
                  value={grade.participants ?? ''}
                  onChange={(event) =>
                    updateGrade(updateRecord, grade.id, {
                      participants: event.target.value ? Number(event.target.value) : undefined,
                    })
                  }
                  className={inputClass}
                  placeholder="Participants"
                />
              </div>
              <div className="mt-2 grid gap-2 md:grid-cols-5">
                {(
                  [
                    'risk_of_bias',
                    'inconsistency',
                    'indirectness',
                    'imprecision',
                    'publication_bias',
                  ] as const
                ).map((domain) => (
                  <label key={domain} className="text-[10px] text-[var(--text-tertiary)]">
                    {domain.replaceAll('_', ' ')}
                    <select
                      value={grade[domain].concern}
                      onChange={(event) =>
                        updateGrade(updateRecord, grade.id, {
                          [domain]: {
                            ...grade[domain],
                            concern: event.target.value as GradeConcern,
                          },
                        })
                      }
                      className={`${inputClass} mt-1`}
                    >
                      <option value="not_serious">Not serious</option>
                      <option value="serious">Serious</option>
                      <option value="very_serious">Very serious</option>
                    </select>
                    <input
                      value={grade[domain].explanation}
                      onChange={(event) =>
                        updateGrade(updateRecord, grade.id, {
                          [domain]: { ...grade[domain], explanation: event.target.value },
                        })
                      }
                      className={`${inputClass} mt-1`}
                      placeholder="Reason"
                    />
                  </label>
                ))}
              </div>
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}

function PrismaEditor({
  flow,
  record,
  updateRecord,
}: {
  flow: PrismaFlow;
  record: SystematicReviewRecord;
  updateRecord: (update: (record: SystematicReviewRecord) => SystematicReviewRecord) => void;
}) {
  const stages: Array<[string, number]> = [
    ['Records identified', flow.records_identified],
    ['Duplicates removed', flow.duplicates_removed],
    ['Records screened', flow.records_screened],
    ['Records excluded', flow.records_excluded],
    ['Reports sought', flow.reports_sought],
    ['Reports not retrieved', flow.reports_not_retrieved],
    ['Reports assessed', flow.reports_assessed],
    ['Reports excluded', flow.reports_excluded],
    ['Studies included', flow.studies_included],
  ];
  return (
    <div className="p-4">
      <div className="grid gap-2 sm:grid-cols-3">
        {stages.map(([label, value]) => (
          <div key={label} className="border-b border-[var(--border-primary)] px-2 py-3">
            <div className="text-[10px] text-[var(--text-tertiary)]">{label}</div>
            <div className="mt-1 text-lg font-semibold text-[var(--text-primary)]">{value}</div>
          </div>
        ))}
      </div>
      <div className="mt-5 grid gap-3 sm:grid-cols-3">
        {(
          [
            ['additional_identified', 'Additional records'],
            ['duplicates_removed', 'Duplicates removed'],
            ['reports_not_retrieved', 'Reports not retrieved'],
          ] as const
        ).map(([field, label]) => (
          <Field key={field} label={label}>
            <input
              type="number"
              min="0"
              value={record.prisma[field]}
              onChange={(event) =>
                updateRecord((current) => ({
                  ...current,
                  prisma: { ...current.prisma, [field]: Number(event.target.value) || 0 },
                }))
              }
              className={inputClass}
            />
          </Field>
        ))}
      </div>
    </div>
  );
}

function updateGrade(
  updateRecord: (update: (record: SystematicReviewRecord) => SystematicReviewRecord) => void,
  gradeId: string,
  patch: Partial<SystematicReviewRecord['grade'][number]>
) {
  updateRecord((record) => ({
    ...record,
    grade: record.grade.map((grade) => (grade.id === gradeId ? { ...grade, ...patch } : grade)),
  }));
}

function defaultRiskDomains(tool: 'rob2' | 'robins_i' | 'custom') {
  const domains =
    tool === 'robins_i'
      ? [
          'Confounding',
          'Selection of participants',
          'Classification of interventions',
          'Deviations from intended interventions',
          'Missing data',
          'Measurement of outcomes',
          'Selection of the reported result',
        ]
      : tool === 'rob2'
        ? [
            'Randomization process',
            'Deviations from intended interventions',
            'Missing outcome data',
            'Measurement of the outcome',
            'Selection of the reported result',
          ]
        : ['Custom domain'];
  return domains.map((domain) => ({
    domain,
    judgment: 'low' as RiskJudgment,
    rationale: '',
  }));
}

function replaceAt<T>(values: T[], index: number, value: T): T[] {
  return values.map((item, position) => (position === index ? value : item));
}

function DecisionSelect({
  value,
  onChange,
  disabled,
}: {
  value: ScreeningDecisionValue;
  onChange: (value: ScreeningDecisionValue) => void;
  disabled: boolean;
}) {
  return (
    <select
      disabled={disabled}
      value={value}
      onChange={(event) => onChange(event.target.value as ScreeningDecisionValue)}
      className={inputClass}
    >
      <option value="pending">Pending</option>
      <option value="include">Include</option>
      <option value="exclude">Exclude</option>
      <option value="maybe">Maybe</option>
    </select>
  );
}

function LinesField({
  label,
  values,
  onChange,
}: {
  label: string;
  values: string[];
  onChange: (values: string[]) => void;
}) {
  return (
    <Field label={label}>
      <textarea
        value={values.join('\n')}
        onChange={(event) =>
          onChange(
            event.target.value
              .split('\n')
              .map((value) => value.trim())
              .filter(Boolean)
          )
        }
        className={`${inputClass} min-h-24 resize-y py-2`}
      />
    </Field>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="block text-[11px] text-[var(--text-tertiary)]">
      <span className="mb-1 block">{label}</span>
      {children}
    </label>
  );
}

const inputClass =
  'h-8 w-full rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] px-2 text-xs text-[var(--text-primary)] outline-none focus:border-[var(--accent)] disabled:opacity-50';

export default ReviewWorkbench;
