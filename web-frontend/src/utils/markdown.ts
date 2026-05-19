/**
 * Lightweight markdown-to-HTML renderer.
 * Handles: code blocks, inline code, bold, italic, headers, lists, links, blockquotes, paragraphs.
 * Output is trusted (from backend agent), safe for dangerouslySetInnerHTML.
 */

export function renderMarkdown(text: string): string {
  // Normalize line endings
  text = text.replace(/\r\n/g, '\n').replace(/\r/g, '\n');

  // Protect code blocks first
  const codeBlocks: string[] = [];
  let processed = text.replace(/```[^\S\r\n]*(\S*)[^\S\r\n]*[\r\n]+([\s\S]*?)```/g, (_, lang, code) => {
    const idx = codeBlocks.length;
    const escaped = escapeHtml(code.trimEnd());
    const langLabel = lang || 'text';
    codeBlocks.push(
      `<div class="md-pre-wrap"><div class="md-pre-header"><span>${langLabel}</span><button class="md-pre-copy" onclick="navigator.clipboard.writeText(this.closest('.md-pre-wrap').querySelector('code').textContent)">复制</button></div><pre><code>${escaped}</code></pre></div>`
    );
    return `\x00CB${idx}\x00`;
  });

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
  result = result.replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>');
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
 * Strip dangerous HTML that could execute scripts when rendered via
 * dangerouslySetInnerHTML.  Uses regex (no DOM dependency) and is applied
 * after all markdown-to-HTML conversion is complete.
 *
 * Removes:
 *  - <script>…</script> (including self-closing variants)
 *  - <iframe>…</iframe>
 *  - All on* event-handler attributes (onclick, onload, onerror, …)
 *  - javascript: / vbscript: URLs in href / src attributes
 */
function sanitizeHtml(html: string): string {
  // Protect the intentionally-safe Copy-button onclick by replacing it
  // with a placeholder before stripping on* attributes.
  const SAFE_ONCLICK_TOKEN = '\x00SAFE_ONCLICK\x00';
  let safe = html.replace(
    /onclick="navigator\.clipboard\.writeText\(this\.closest\(['"][^'"]*['"]\)\.querySelector\(['"][^'"]*['"]\)\.textContent\)"/g,
    SAFE_ONCLICK_TOKEN
  );

  // 1. Remove <script>…</script> (and self-closing <script…/>)
  safe = safe.replace(/<script\b[^>]*>[\s\S]*?<\/script\s*>/gi, '');
  safe = safe.replace(/<script\b[^>]*\/>/gi, '');

  // 2. Remove <iframe>…</iframe> (and self-closing)
  safe = safe.replace(/<iframe\b[^>]*>[\s\S]*?<\/iframe\s*>/gi, '');
  safe = safe.replace(/<iframe\b[^>]*\/>/gi, '');

  // 3. Strip ALL on* event-handler attributes
  safe = safe.replace(/\s+on\w+\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+)/gi, '');

  // 4. Neutralise javascript: / vbscript: in href and src attributes
  safe = safe.replace(
    /((?:href|src)\s*=\s*)(["'])(?:\s*(?:javascript|vbscript)\s*:)/gi,
    '$1$2#blocked:'
  );
  // Also handle unquoted variant
  safe = safe.replace(
    /((?:href|src)\s*=\s*)(?:\s*(?:javascript|vbscript)\s*:)/gi,
    '$1#blocked:'
  );

  // Restore the safe Copy-button onclick
  safe = safe.replace(/\x00SAFE_ONCLICK\x00/g,
    'onclick="navigator.clipboard.writeText(this.closest(\'.md-pre-wrap\').querySelector(\'code\').textContent)"'
  );

  return safe;
}
