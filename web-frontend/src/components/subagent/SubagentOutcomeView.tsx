import { AlertTriangle, CheckCircle2, FileText } from 'lucide-react';
import type { SubagentOutcome } from '../../generated';
import MarkdownContent from '../common/MarkdownContent';

interface Props {
  outcome?: SubagentOutcome;
  content?: string;
  error?: string;
  maxHeight?: number;
}

function normalizedText(value: string): string {
  return value.trim().replaceAll(/\s+/g, ' ');
}

export function SubagentOutcomeView({ outcome, content, error, maxHeight }: Props) {
  const displayText = content?.trim() || outcome?.summary.trim() || '';
  const errorText = error?.trim() || '';
  const visibleError =
    errorText && normalizedText(errorText) !== normalizedText(displayText) ? errorText : '';
  const seenRemaining = new Set(
    [displayText, errorText].filter(Boolean).map((item) => normalizedText(item))
  );
  const remainingWork = (outcome?.remaining_work ?? []).filter((item) => {
    const normalized = normalizedText(item);
    if (!normalized || seenRemaining.has(normalized)) return false;
    seenRemaining.add(normalized);
    return true;
  });
  if (!displayText && !visibleError && !outcome) return null;

  return (
    <div className="space-y-3 text-[11px] text-[var(--text-secondary)]">
      {displayText && (
        <MarkdownContent content={displayText} className="text-sm" maxHeight={maxHeight} />
      )}

      {visibleError && (
        <div role="alert" className="text-sm text-[var(--color-error)]">
          {visibleError}
        </div>
      )}

      {(outcome?.verification.length ?? 0) > 0 && (
        <section className="space-y-1.5">
          <div className="flex items-center gap-1 text-[10px] font-medium text-[var(--text-tertiary)]">
            <CheckCircle2 size={11} />
            验证
          </div>
          {outcome?.verification.map((item) => (
            <div key={`${item.check}-${item.source}`}>
              <span className="font-medium text-[var(--text-primary)]">{item.check}</span>
              <span className="ml-1 text-[var(--text-tertiary)]">
                {item.status} · {item.source}
              </span>
              {item.details ? (
                <div className="mt-0.5 text-[var(--text-tertiary)]">{item.details}</div>
              ) : null}
            </div>
          ))}
        </section>
      )}

      {(outcome?.artifacts.length ?? 0) > 0 && (
        <section className="space-y-1.5">
          <div className="flex items-center gap-1 text-[10px] font-medium text-[var(--text-tertiary)]">
            <FileText size={11} />
            产物
          </div>
          {outcome?.artifacts.map((artifact) => (
            <div key={artifact.path} className="break-all font-mono text-[10px]">
              {artifact.available ? 'available' : 'missing'} · {artifact.path}
            </div>
          ))}
        </section>
      )}

      {remainingWork.length > 0 && (
        <section className="space-y-1.5">
          <div className="flex items-center gap-1 text-[10px] font-medium text-[var(--color-warning)]">
            <AlertTriangle size={11} />
            未完成
          </div>
          {remainingWork.map((item) => (
            <div key={item} className="text-[var(--color-warning)]">
              {item}
            </div>
          ))}
        </section>
      )}
    </div>
  );
}
