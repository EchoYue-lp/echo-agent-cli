import { describe, expect, it } from 'vitest';
import type { Workspace } from '../api/endpoints';
import { workspaceIsRemote, workspaceLocationLabel } from './workspacePresentation';

const local: Workspace = {
  id: 'current',
  name: 'Current',
  root: '/product-data',
  project_root: '/stale-projection',
  kind: { type: 'general' },
  metadata: { tags: [], project_location: { kind: 'local' as const, path: '/explicit-code' } },
  product_data_generation: 'current-generation',
  created_at: '2026-10-28T00:00:00Z',
  last_active: '2026-10-28T00:00:00Z',
};
describe('当前Workspace位置权威', () => {
  it('本地显示只读取显式location，不猜project_root或产品目录', () => {
    expect(workspaceLocationLabel(local)).toBe('/explicit-code');
    expect(workspaceIsRemote(local)).toBe(false);
  });
  it('远程显示使用原verified执行身份，仍保留用户配置', () => {
    const remote: Workspace = {
      ...local,
      metadata: {
        tags: [],
        project_location: { kind: 'ssh', host: 'alias', user: null, port: null, path: './project' },
        project_execution_location: {
          kind: 'ssh',
          host: 'actual.example',
          user: 'echo',
          port: 2222,
          path: '/actual/project',
        },
      },
    };
    expect(workspaceLocationLabel(remote)).toBe('echo@actual.example:2222 · /actual/project');
    expect(workspaceIsRemote(remote)).toBe(true);
    expect(remote.metadata.project_location.path).toBe('./project');
  });
});
