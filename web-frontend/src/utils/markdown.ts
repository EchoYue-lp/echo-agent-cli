/**
 * Lightweight markdown-to-HTML renderer.
 * Handles: code blocks, inline code, bold, italic, headers, lists, links, blockquotes, paragraphs.
 * Output is trusted (from backend agent), safe for dangerouslySetInnerHTML.
 */

export function renderMarkdown(text: string): string {
  // Protect code blocks first
  const codeBlocks: string[] = [];
  let processed = text.replace(/```(\w*)\n([\s\S]*?)```/g, (_, lang, code) => {
    const idx = codeBlocks.length;
    const escaped = escapeHtml(code.trimEnd());
    const langLabel = lang || 'text';
    codeBlocks.push(
      `<div class="md-pre-wrap"><div class="md-pre-header"><span>${langLabel}</span><button class="md-pre-copy" onclick="navigator.clipboard.writeText(this.closest('.md-pre-wrap').querySelector('code').textContent)">Copy</button></div><pre><code>${escaped}</code></pre></div>`
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
    if (line.startsWith('> ')) {
      flushParagraph();
      flushList();
      result.push(`<blockquote>${inlineFormat(line.slice(2))}</blockquote>`);
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
