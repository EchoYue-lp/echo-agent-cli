import type { Workspace } from '../api/endpoints';

/** Code location and local product-data storage deliberately have different identities. */
export function workspaceLocationLabel(workspace: Workspace): string {
  const location =
    workspace.metadata.project_execution_location ?? workspace.metadata.project_location;
  if (location.kind === 'ssh') {
    const address = `${location.user ? `${location.user}@` : ''}${location.host}${location.port ? `:${location.port}` : ''}`;
    return `${address} · ${location.path}`;
  }
  return location.path;
}

export function workspaceIsRemote(workspace: Workspace): boolean {
  return (
    (workspace.metadata.project_execution_location ?? workspace.metadata.project_location).kind ===
    'ssh'
  );
}
