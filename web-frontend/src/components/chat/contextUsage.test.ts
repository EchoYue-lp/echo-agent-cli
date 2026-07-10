import { describe, expect, it } from 'vitest';
import { computeContextUsage, estimateDraftTokens } from './contextUsage';

describe('context usage', () => {
  it('adds only the unsent draft and attachments to the provider baseline', () => {
    const draftTokens = estimateDraftTokens('继续检查', [
      { name: 'report.md', mime_type: 'text/markdown' },
    ]);
    const usage = computeContextUsage(1_000, draftTokens, 10_000);

    expect(draftTokens).toBeGreaterThan(0);
    expect(usage.used).toBe(1_000 + draftTokens);
    expect(usage.source).toBe('reported');
    expect(usage.tier).toBe('normal');
  });

  it('shows unknown before the first provider report without a draft', () => {
    expect(computeContextUsage(null, 0, 128_000)).toEqual({
      used: null,
      pct: null,
      source: 'unknown',
      tier: 'unknown',
    });
  });

  it('shows draft-only state after compression clears the provider snapshot', () => {
    const draftTokens = estimateDraftTokens('new prompt', []);
    const usage = computeContextUsage(null, draftTokens, 128_000);

    expect(usage.used).toBe(draftTokens);
    expect(usage.source).toBe('draft-only');
    expect(usage.pct).not.toBeNull();
  });
});
