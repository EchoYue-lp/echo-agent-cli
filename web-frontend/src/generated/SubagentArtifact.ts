// Framework-owned SubagentArtifact contract exposed by the EKO IPC surface.

export type SubagentArtifact = {
  path: string;
  kind: string;
  bytes: bigint | null;
  sha256: string | null;
  producer_execution_id: string | null;
  available: boolean;
};
