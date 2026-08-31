// Framework-owned SubagentOutcome contract exposed by the EKO IPC surface.
import type { SubagentArtifact } from './SubagentArtifact';
import type { SubagentEvidence } from './SubagentEvidence';
import type { SubagentStatus } from './SubagentStatus';
import type { SubagentTouchedFiles } from './SubagentTouchedFiles';
import type { SubagentVerification } from './SubagentVerification';

export type SubagentOutcome = {
  contract_version: number;
  status: SubagentStatus;
  summary: string;
  artifacts: Array<SubagentArtifact>;
  evidence: Array<SubagentEvidence>;
  verification: Array<SubagentVerification>;
  remaining_work: Array<string>;
  touched_files: SubagentTouchedFiles;
};
