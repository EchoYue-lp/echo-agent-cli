import { describe, expect, it } from 'vitest';
import { isChromeExtensionId } from './ChromeSetupDialog';

describe('isChromeExtensionId', () => {
  it('accepts a 32-character Chrome extension id', () => {
    expect(isChromeExtensionId('abcdefghijklmnopabcdefghijklmnop')).toBe(true);
  });

  it('rejects invalid lengths and characters', () => {
    expect(isChromeExtensionId('abcdefghijklmnop')).toBe(false);
    expect(isChromeExtensionId('zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz')).toBe(false);
  });
});
