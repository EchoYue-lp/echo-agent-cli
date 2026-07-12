export const TOOL_OUTPUT_MAX_CHARS = 131_072;
export const TOOL_OUTPUT_MAX_BYTES = 131_072;
export const TOOL_OUTPUT_MAX_LINES = 1_000;

const encoder = new TextEncoder();

export function appendBoundedToolOutput(
  current: string,
  chunk: string
): { value: string; truncated: boolean } {
  const characters = Array.from(current + chunk);
  let byteCount = 0;
  let lineCount = 1;
  let start = characters.length;

  while (start > 0) {
    const character = characters[start - 1];
    if (character == null) break;
    const characterBytes = encoder.encode(character).byteLength;
    const nextLines = lineCount + (character === '\n' ? 1 : 0);
    const keptChars = characters.length - start + 1;
    if (
      keptChars > TOOL_OUTPUT_MAX_CHARS ||
      byteCount + characterBytes > TOOL_OUTPUT_MAX_BYTES ||
      nextLines > TOOL_OUTPUT_MAX_LINES
    ) {
      break;
    }
    start -= 1;
    byteCount += characterBytes;
    lineCount = nextLines;
  }

  if (start === 0) return { value: characters.join(''), truncated: false };
  return { value: characters.slice(start).join(''), truncated: true };
}
