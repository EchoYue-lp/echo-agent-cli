import DOMPurify from 'dompurify';

/**
 * Lightweight markdown-to-HTML renderer.
 * Handles: code blocks, inline code, bold, italic, headers, lists, links, blockquotes, paragraphs.
 * Output is sanitized with DOMPurify before being used with dangerouslySetInnerHTML.
 */

export function renderMarkdown(text: string): string {
  // Normalize line endings
  text = text.replace(/\r\n/g, '\n').replace(/\r/g, '\n');

  // Protect code blocks first
  const codeBlocks: string[] = [];
  let processed = text.replace(
    /```[^\S\r\n]*(\S*)[^\S\r\n]*[\r\n]+([\s\S]*?)```/g,
    (_, lang, code) => {
      const idx = codeBlocks.length;
      const escaped = escapeHtml(code.trimEnd());
      const langLabel = lang || 'text';
      codeBlocks.push(
        `<div class="md-pre-wrap"><div class="md-pre-header"><span>${langLabel}</span><button type="button" class="md-pre-copy">复制</button></div><pre><code>${escaped}</code></pre></div>`
      );
      return `\x00CB${idx}\x00`;
    }
  );

  // Split into lines for block-level processing
  const lines = processed.split('\n');
  const result: string[] = [];
  let inList = false;
  let listType = '';
  let paragraphBuffer: string[] = [];

  const flushParagraph = () => {
    if (paragraphBuffer.length > 0) {
      const content = paragraphBuffer.join('<br>');
      if (content.trim()) {
        result.push(`<p>${content}</p>`);
      }
      paragraphBuffer = [];
    }
  };

  const flushList = () => {
    if (inList) {
      result.push(listType === 'ul' ? '</ul>' : '</ol>');
      inList = false;
    }
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];

    // Code block placeholder
    if (/^\x00CB\d+\x00$/.test(line.trim())) {
      flushParagraph();
      flushList();
      result.push(line.trim());
      continue;
    }

    // Headers
    const headerMatch = line.match(/^(#{1,3})\s+(.+)/);
    if (headerMatch) {
      flushParagraph();
      flushList();
      const level = headerMatch[1].length;
      result.push(`<h${level}>${inlineFormat(headerMatch[2])}</h${level}>`);
      continue;
    }

    // Blockquote
    if (/^>\s?/.test(line)) {
      flushParagraph();
      flushList();
      const content = line.replace(/^>\s?/, '');
      result.push(`<blockquote>${inlineFormat(content)}</blockquote>`);
      continue;
    }

    // Unordered list
    const ulMatch = line.match(/^[\s]*[-*]\s+(.+)/);
    if (ulMatch) {
      flushParagraph();
      if (!inList || listType !== 'ul') {
        flushList();
        result.push('<ul>');
        inList = true;
        listType = 'ul';
      }
      result.push(`<li>${inlineFormat(ulMatch[1])}</li>`);
      continue;
    }

    // Ordered list
    const olMatch = line.match(/^[\s]*\d+\.\s+(.+)/);
    if (olMatch) {
      flushParagraph();
      if (!inList || listType !== 'ol') {
        flushList();
        result.push('<ol>');
        inList = true;
        listType = 'ol';
      }
      result.push(`<li>${inlineFormat(olMatch[1])}</li>`);
      continue;
    }

    // Horizontal rule
    if (/^---+$/.test(line.trim())) {
      flushParagraph();
      flushList();
      result.push('<hr/>');
      continue;
    }

    // Empty line
    if (line.trim() === '') {
      flushParagraph();
      flushList();
      continue;
    }

    // Regular text -> paragraph buffer
    flushList();
    paragraphBuffer.push(inlineFormat(line));
  }

  flushParagraph();
  flushList();

  // Restore code blocks
  let html = result.join('\n');
  html = html.replace(/\x00CB(\d+)\x00/g, (_, idx) => codeBlocks[parseInt(idx)]);

  // Sanitize to remove any dangerous HTML injected via markdown content
  html = sanitizeHtml(html);

  return html;
}

function inlineFormat(text: string): string {
  let result = escapeHtml(text);
  // Inline code
  result = result.replace(/`([^`]+)`/g, '<code>$1</code>');
  // Bold
  result = result.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');
  // Italic
  result = result.replace(/(?<!\*)\*([^*]+?)\*(?!\*)/g, '<em>$1</em>');
  // Links
  result = result.replace(
    /\[([^\]]+)\]\(([^)]+)\)/g,
    '<a href="$2" target="_blank" rel="noopener">$1</a>'
  );
  return result;
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

/**
 * Sanitize HTML using DOMPurify to prevent XSS attacks.
 *
 * SECURITY: `onclick` (and all `on*` event handler attributes) is intentionally
 * NOT in ALLOWED_ATTR. The copy button carries no inline handler — it is
 * identified purely by its `md-pre-copy` class and wired up via a delegated
 * click listener on the render container (see `MarkdownContent`). Allowing
 * `onclick` here would let any LLM-emitted / tool-result text that survives
 * markdown parsing (e.g. `<button onclick="...">`) execute arbitrary JS in the
 * page — the XSS pivot to the Tauri fs/shell capabilities.
 */
function sanitizeHtml(html: string): string {
  return DOMPurify.sanitize(html, {
    ALLOWED_TAGS: [
      'p',
      'br',
      'strong',
      'em',
      'code',
      'pre',
      'div',
      'span',
      'ul',
      'ol',
      'li',
      'blockquote',
      'hr',
      'h1',
      'h2',
      'h3',
      'a',
      'button',
    ],
    ALLOWED_ATTR: ['class', 'href', 'target', 'rel', 'title', 'type'],
    ALLOW_DATA_ATTR: false,
  });
}
