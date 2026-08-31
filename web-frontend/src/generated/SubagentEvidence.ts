// Framework-owned SubagentOutcome evidence contract.

import type { SubagentEvidenceSource } from './SubagentEvidenceSource';

export type SubagentEvidence = {
  kind: string;
  subject: string;
  outcome?: string | null;
  details: string;
  source: SubagentEvidenceSource;
  attributes: unknown;
};
