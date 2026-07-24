import { AlertTriangle, CheckCircle2, FileText } from 'lucide-react';
import type { SubagentTaskResult } from '../../generated';
import MarkdownContent from '../common/MarkdownContent';

interface Props {
  result?: SubagentTaskResult;
  content?: string;
  maxHeight?: number;
}

export function SubagentResultView({ result, content, maxHeight }: Props) {
  const displayText = content?.trim() || result?.summary.trim() || '';
  if (!displayText && !result) return null;

  return (
    <div className="space-y-3 text-[11px] text-[var(--text-secondary)]">
      {displayText && (
        <MarkdownContent content={displayText} className="text-sm" maxHeight={maxHeight} />
      )}

      {(result?.verification.length ?? 0) > 0 && (
        <section className="space-y-1.5">
          <div className="flex items-center gap-1 text-[10px] font-medium text-[var(--text-tertiary)]">
            <CheckCircle2 size={11} />
            验证
          </div>
          {result?.verification.map((item) => (
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

      {(result?.artifacts.length ?? 0) > 0 && (
        <section className="space-y-1.5">
          <div className="flex items-center gap-1 text-[10px] font-medium text-[var(--text-tertiary)]">
            <FileText size={11} />
            产物
          </div>
          {result?.artifacts.map((artifact) => (
            <div key={artifact.path} className="break-all font-mono text-[10px]">
              {artifact.available ? 'available' : 'missing'} · {artifact.path}
            </div>
          ))}
        </section>
      )}

      {(result?.remaining_work.length ?? 0) > 0 && (
        <section className="space-y-1.5">
          <div className="flex items-center gap-1 text-[10px] font-medium text-[var(--color-warning)]">
            <AlertTriangle size={11} />
            未完成
          </div>
          {result?.remaining_work.map((item) => (
            <div key={item} className="text-[var(--color-warning)]">
              {item}
            </div>
          ))}
        </section>
      )}
    </div>
  );
}
