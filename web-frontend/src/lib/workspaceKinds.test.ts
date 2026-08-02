import { describe, expect, it } from 'vitest';
import { getWorkspaceKind, WORKSPACE_KINDS } from './workspaceKinds';

describe('workspace kinds', () => {
  it('shows one data-analysis option while resolving the legacy data alias', () => {
    expect(WORKSPACE_KINDS.filter((kind) => kind.label === '数据分析')).toHaveLength(1);
    expect(WORKSPACE_KINDS.some((kind) => kind.value === 'data')).toBe(false);
    expect(getWorkspaceKind('data')).toBe(getWorkspaceKind('data_analysis'));
  });
});
