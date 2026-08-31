// Framework-owned SubagentVerification contract exposed by the EKO IPC surface.
import type { SubagentEvidenceSource } from './SubagentEvidenceSource';
import type { SubagentVerificationStatus } from './SubagentVerificationStatus';

export type SubagentVerification = {
  check: string;
  status: SubagentVerificationStatus;
  details: string;
  source: SubagentEvidenceSource;
};
