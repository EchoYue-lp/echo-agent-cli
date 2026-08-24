import type { ProductDataScope, Workspace } from '../api/endpoints';
import { GLOBAL_WORKSPACE_ID } from './viewAddress';

export const GLOBAL_WORKSPACE_GENERATION = 'global';

export function productDataScope(workspace: Workspace | null | undefined): ProductDataScope {
  return workspace
    ? {
        workspaceId: workspace.id,
        workspaceGeneration: workspace.product_data_generation,
      }
    : {
        workspaceId: GLOBAL_WORKSPACE_ID,
        workspaceGeneration: GLOBAL_WORKSPACE_GENERATION,
      };
}

export function productDataScopeKey(scope: ProductDataScope): string {
  return JSON.stringify([scope.workspaceId, scope.workspaceGeneration]);
}

export function sameProductDataScope(left: ProductDataScope, right: ProductDataScope): boolean {
  return (
    left.workspaceId === right.workspaceId && left.workspaceGeneration === right.workspaceGeneration
  );
}
