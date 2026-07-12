import { describe, expect, it } from 'vitest';
import {
  appendBoundedToolOutput,
  TOOL_OUTPUT_MAX_BYTES,
  TOOL_OUTPUT_MAX_LINES,
} from './toolOutput';

describe('bounded tool output', () => {
  it('keeps a UTF-8-safe tail under the byte limit', () => {
    const result = appendBoundedToolOutput('', `prefix${'🙂'.repeat(TOOL_OUTPUT_MAX_BYTES)}`);
    expect(result.truncated).toBe(true);
    expect(new TextEncoder().encode(result.value).byteLength).toBeLessThanOrEqual(
      TOOL_OUTPUT_MAX_BYTES
    );
    expect(result.value.endsWith('🙂')).toBe(true);
  });

  it('keeps at most the configured number of recent lines', () => {
    const input = Array.from({ length: TOOL_OUTPUT_MAX_LINES + 5 }, (_, index) => `${index}`).join(
      '\n'
    );
    const result = appendBoundedToolOutput('', input);
    expect(result.truncated).toBe(true);
    expect(result.value.split('\n')).toHaveLength(TOOL_OUTPUT_MAX_LINES);
    expect(result.value.endsWith(`${TOOL_OUTPUT_MAX_LINES + 4}`)).toBe(true);
  });
});
