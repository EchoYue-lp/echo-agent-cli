import type { AgentAddress } from '../generated';

export const GLOBAL_WORKSPACE_ID = 'global';

export type ViewAddress = AgentAddress;

export function workspaceIdForView(workspaceId: string | null | undefined): string {
  return workspaceId?.trim() || GLOBAL_WORKSPACE_ID;
}

export function viewAddress(workspaceId: string, conversationId: string): ViewAddress {
  return {
    workspace_id: workspaceIdForView(workspaceId),
    conversation_id: conversationId,
  };
}

export function viewAddressKey(address: ViewAddress): string {
  return JSON.stringify([address.workspace_id, address.conversation_id]);
}

export function sameViewAddress(left: ViewAddress | null, right: ViewAddress | null): boolean {
  return (
    left === right ||
    (left !== null &&
      right !== null &&
      left.workspace_id === right.workspace_id &&
      left.conversation_id === right.conversation_id)
  );
}
