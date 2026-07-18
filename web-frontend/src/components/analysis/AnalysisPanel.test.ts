import { describe, expect, it } from 'vitest';
import { formatAnalysisBytes } from './AnalysisPanel';

describe('formatAnalysisBytes', () => {
  it('formats analysis artifacts without losing small-file precision', () => {
    expect(formatAnalysisBytes(512)).toBe('512 B');
    expect(formatAnalysisBytes(1536)).toBe('1.5 KiB');
    expect(formatAnalysisBytes(2 * 1024 * 1024)).toBe('2.0 MiB');
  });
});
