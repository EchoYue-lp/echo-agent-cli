import { describe, expect, it } from 'vitest';
import { boundRightWorkspaceWidth } from './rightWorkspaceStore';

describe('boundRightWorkspaceWidth', () => {
  it('keeps the right workspace within usable desktop bounds', () => {
    expect(boundRightWorkspaceWidth(200)).toBe(380);
    expect(boundRightWorkspaceWidth(560)).toBe(560);
    expect(boundRightWorkspaceWidth(1000)).toBe(760);
  });
});
