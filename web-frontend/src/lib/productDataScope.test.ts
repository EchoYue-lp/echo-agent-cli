import { describe, expect, it } from 'vitest';
import { productDataScope, productDataScopeKey } from './productDataScope';
import type { Workspace } from '../api/endpoints';

function workspace(createdAt: string, generation = `opaque:${createdAt}`): Workspace {
  return {
    id: 'same-workspace',
    name: 'Same workspace',
    root: '/tmp/same-workspace',
    kind: { type: 'general' },
    metadata: { tags: [], project_root_revision: 0 },
    product_data_generation: generation,
    created_at: createdAt,
    last_active: createdAt,
  };
}

describe('productDataScope', () => {
  it('distinguishes a deleted and recreated workspace with the same id', () => {
    const stale = productDataScope(workspace('2026-08-24T10:00:00+08:00'));
    const recreated = productDataScope(workspace('2026-08-24T10:01:00+08:00'));

    expect(stale.workspaceId).toBe(recreated.workspaceId);
    expect(productDataScopeKey(stale)).not.toBe(productDataScopeKey(recreated));
  });

  it('invalidates the scope when a linked project root changes', () => {
    const initial = workspace('2026-08-24T10:00:00+08:00');
    const linked = {
      ...initial,
      project_root: '/tmp/linked-project',
      metadata: { ...initial.metadata, project_root_revision: 1 },
      product_data_generation: 'opaque:linked-project-incarnation',
    };

    expect(productDataScopeKey(productDataScope(initial))).not.toBe(
      productDataScopeKey(productDataScope(linked))
    );
  });

  it('uses one stable global scope identity', () => {
    expect(productDataScope(null)).toEqual({
      workspaceId: 'global',
      workspaceGeneration: 'global',
    });
  });

  it('passes through a backend token without normalizing its timestamp text', () => {
    const token = '["2026-08-24T00:00:00.000000000Z",7]';
    const scope = productDataScope(workspace('2026-08-24T08:00:00+08:00', token));
    expect(scope.workspaceGeneration).toBe(token);
  });
});
