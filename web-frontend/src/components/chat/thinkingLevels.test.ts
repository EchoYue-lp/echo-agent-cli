import { describe, expect, it } from 'vitest';
import { isKnownThinkingLevel, thinkingLevelOptions } from './thinkingLevels';

describe('thinking level options', () => {
  it('preserves all six GPT-5.6 levels without merging max into xhigh', () => {
    expect(thinkingLevelOptions(['none', 'low', 'medium', 'high', 'xhigh', 'max'])).toEqual([
      { id: 'auto', label: '自动' },
      { id: 'none', label: '关闭' },
      { id: 'low', label: '低' },
      { id: 'medium', label: '中' },
      { id: 'high', label: '高' },
      { id: 'xhigh', label: '很高' },
      { id: 'max', label: '最高' },
    ]);
  });

  it('returns only auto for unknown or model-managed profiles', () => {
    expect(thinkingLevelOptions([])).toEqual([{ id: 'auto', label: '自动' }]);
  });

  it('does not recognize the unsupported ultra alias', () => {
    expect(isKnownThinkingLevel('max')).toBe(true);
    expect(isKnownThinkingLevel('ultra')).toBe(false);
  });
});
