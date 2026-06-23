import { useState, memo } from 'react';
import { Brain, ChevronDown, ChevronRight } from 'lucide-react';
import MarkdownContent from '../common/MarkdownContent';

interface ThinkingSegmentProps {
  /** 1-based index among thinking segments in this message */
  index: number;
  /** Total thinking segments in this message (for "思考 1/3" labeling) */
  total: number;
  content: string;
  /** True while the parent message is still streaming */
  isStreaming?: boolean;
}

/**
 * One "thinking" segment in the inline one-stream layout.
 * Collapsible; expanded by default while streaming, collapsed after streaming ends.
 */
export const ThinkingSegment = memo(function ThinkingSegment({
  index,
  total,
  content,
  isStreaming,
}: ThinkingSegmentProps) {
  const [expanded, setExpanded] = useState(Boolean(isStreaming));

  const label = total > 1 ? `思考 ${index}/${total}` : '思考';

  return (
    <div
      className="my-1 rounded-md border-l-2 border-[var(--color-purple)] bg-[var(--bg-primary)] px-3 py-1.5"
    >
      <button
        onClick={() => setExpanded((e) => !e)}
        className="flex w-full items-center gap-1.5 text-left"
      >
        {expanded ? (
          <ChevronDown size={11} className="text-[var(--text-tertiary)]" />
        ) : (
          <ChevronRight size={11} className="text-[var(--text-tertiary)]" />
        )}
        <Brain
          size={11}
          className={isStreaming ? 'text-[var(--color-purple)] animate-pulse' : 'text-[var(--color-purple)]'}
        />
        <span className="text-[10px] font-medium text-[var(--color-purple)]">{label}</span>
      </button>
      {expanded && (
        <div className="mt-1.5 leading-relaxed text-[var(--text-secondary)]">
          <MarkdownContent content={content} className="text-sm" />
        </div>
      )}
    </div>
  );
});
