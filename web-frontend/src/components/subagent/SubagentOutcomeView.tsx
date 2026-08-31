import { AlertTriangle, CheckCircle2, FileText } from 'lucide-react';
import type { SubagentOutcome } from '../../generated';
import MarkdownContent from '../common/MarkdownContent';

interface Props {
  outcome?: SubagentOutcome;
  content?: string;
  maxHeight?: number;
}

export function SubagentOutcomeView({ outcome, content, maxHeight }: Props) {
  const displayText = content?.trim() || outcome?.summary.trim() || '';
  if (!displayText && !outcome) return null;

  return (
    <div className="space-y-3 text-[11px] text-[var(--text-secondary)]">
      {displayText && (
        <MarkdownContent content={displayText} className="text-sm" maxHeight={maxHeight} />
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

      {(outcome?.remaining_work.length ?? 0) > 0 && (
        <section className="space-y-1.5">
          <div className="flex items-center gap-1 text-[10px] font-medium text-[var(--color-warning)]">
            <AlertTriangle size={11} />
            未完成
          </div>
          {outcome?.remaining_work.map((item) => (
            <div key={item} className="text-[var(--color-warning)]">
              {item}
            </div>
          ))}
        </section>
      )}
    </div>
  );
}
