//! Markdown render container backed by `react-markdown` + `remark-gfm`.
//!
//! Replaces the former hand-rolled parser (`utils/markdown.ts`). react-markdown
//! is secure by default (does not render raw HTML), supports GFM tables / task
//! lists / strikethrough / autolinks, and handles nested lists correctly.
//!
//! The code-block copy button is rendered via a custom `code` component (not a
//! delegated document-level listener), so it stays scoped to this instance.

import { memo, useState, type CSSProperties } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

interface Props {
  /** Raw markdown source (LLM / tool output). */
  content: string;
  /** Extra class on the wrapper div. */
  className?: string;
  /** When set, the content area is capped at this height and scrolls vertically. */
  maxHeight?: number | string;
}

/** Detect a fenced code block language from the className like "language-rust". */
function langFromClassName(className?: string): string {
  if (!className) return 'text';
  const m = /language-(\S+)/.exec(className);
  return m ? m[1] : 'text';
}

function CodeBlock({ className, children }: { className?: string; children?: React.ReactNode }) {
  const [copied, setCopied] = useState(false);
  const lang = langFromClassName(className);
  // children is the raw code text for fenced blocks (react-markdown passes a
  // string). Normalize newlines for display and copying.
  const text = String(children ?? '').replace(/\n$/, '');

  const copy = () => {
    navigator.clipboard.writeText(text).then(
      () => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1200);
      },
      () => {},
    );
  };

  return (
    <div className="md-pre-wrap">
      <div className="md-pre-header">
        <span>{lang}</span>
        <button type="button" className="md-pre-copy" onClick={copy}>
          {copied ? '已复制' : '复制'}
        </button>
      </div>
      <pre>
        <code className={className}>{text}</code>
      </pre>
    </div>
  );
}

/**
 * Inline `code` rendering — no wrapper, just styled inline code.
 * Kept simple so it flows inline within paragraphs / list items.
 */
function InlineCode({ children }: { children?: React.ReactNode }) {
  return <code className="md-inline-code">{children}</code>;
}

const MarkdownContent = memo(function MarkdownContent({
  content,
  className,
  maxHeight,
}: Props) {
  const wrapperStyle: CSSProperties = {};
  if (maxHeight) {
    wrapperStyle.maxHeight = maxHeight;
    wrapperStyle.overflowY = 'auto';
  }

  return (
    <div className={`md-content ${className ?? ''}`.trim()} style={wrapperStyle}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          // Distinguish fenced code blocks (block, with header/copy) from
          // inline code. react-markdown renders both via the `code` component;
          // `inline` was removed in v9, so we detect block vs inline by whether
          // the parent is a `pre` — but react-markdown v9 always wraps block
          // code in <pre><code>. We render the wrapper ourselves and return
          // null for the wrapping `pre` to avoid double-nesting.
          pre({ children }) {
            // children is the <CodeBlock> element we produced via `code`.
            // Return it directly; we don't want react-markdown's own <pre>.
            return <>{children}</>;
          },
          code(props) {
            const { className: cn, children: ch } = props;
            // Block code: react-markdown passes a language-* className from the
            // fence. Inline code has no language class and is short. Heuristic:
            // if there's a language- class OR the content contains a newline,
            // treat as a block.
            const isBlock = (cn && /language-/.test(cn)) || String(ch).includes('\n');
            return isBlock ? (
              <CodeBlock className={cn}>{ch}</CodeBlock>
            ) : (
              <InlineCode>{ch}</InlineCode>
            );
          },
          // Target _blank for links, keep them in-place otherwise.
          a({ href, children }) {
            return (
              <a href={href} target="_blank" rel="noopener noreferrer">
                {children}
              </a>
            );
          },
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
});

export default MarkdownContent;
