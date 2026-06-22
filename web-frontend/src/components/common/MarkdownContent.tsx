//! Safe markdown render container.
//!
//! Wraps the sanitized HTML from `renderMarkdown` and wires the code-block copy
//! button via a single delegated click listener — NOT an inline `onclick`
//! handler (which would re-open the XSS pivot closed in `utils/markdown.ts`).
//!
//! The copy button is identified purely by its `md-pre-copy` class; the
//! associated `<code>` is found by walking up to the enclosing `.md-pre-wrap`.

import { useEffect, useRef } from 'react';
import { renderMarkdown } from '../../utils/markdown';

interface Props {
  /** Raw markdown source (LLM / tool output). */
  content: string;
  /** Extra class on the wrapper div. */
  className?: string;
  /** Extra inline style on the wrapper div. */
  style?: React.CSSProperties;
  /** When set, the content area is capped at this height and scrolls vertically. */
  maxHeight?: number | string;
}

export default function MarkdownContent({ content, className, style, maxHeight }: Props) {
  const ref = useRef<HTMLDivElement>(null);

  const containerStyle: React.CSSProperties = maxHeight
    ? { ...style, maxHeight, overflowY: 'auto' }
    : { ...style };

  useEffect(() => {
    const root = ref.current;
    if (!root) return;

    // Delegated click handler: one listener for all current and future copy
    // buttons within this container. Avoids per-button listeners (which would
    // need re-binding on every streaming token update) and avoids inline
    // onclick (the XSS vector).
    const onClick = (e: MouseEvent) => {
      const target = e.target as HTMLElement | null;
      const btn = target?.closest<HTMLButtonElement>('.md-pre-copy');
      if (!btn) return;
      const wrap = btn.closest<HTMLElement>('.md-pre-wrap');
      const codeEl = wrap?.querySelector('code');
      const text = codeEl?.textContent ?? '';
      // Mark the button so the user sees the copy happened.
      const prev = btn.textContent;
      navigator.clipboard.writeText(text).then(
        () => {
          btn.textContent = '已复制';
          window.setTimeout(() => {
            btn.textContent = prev;
          }, 1200);
        },
        () => {
          btn.textContent = '复制失败';
          window.setTimeout(() => {
            btn.textContent = prev;
          }, 1200);
        },
      );
    };

    root.addEventListener('click', onClick);
    return () => root.removeEventListener('click', onClick);
  }, [content]);

  return (
    <div
      ref={ref}
      className={className}
      style={containerStyle}
      dangerouslySetInnerHTML={{ __html: renderMarkdown(content) }}
    />
  );
}
