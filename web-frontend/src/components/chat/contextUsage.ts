export interface DraftAttachment {
  name: string;
  mime_type: string;
}

export type ContextUsageSource = 'reported' | 'draft-only' | 'unknown';
export type ContextUsageTier = 'normal' | 'high' | 'critical' | 'unknown';

export interface ContextUsage {
  used: number | null;
  pct: number | null;
  source: ContextUsageSource;
  tier: ContextUsageTier;
}

function estimateTextTokens(text: string): number {
  if (!text) return 0;

  let dense = 0;
  let sparse = 0;
  for (const char of Array.from(text)) {
    if (/[\u3040-\u30ff\u3400-\u9fff\uf900-\ufaff\uac00-\ud7af]/u.test(char)) {
      dense += 1;
    } else {
      sparse += 1;
    }
  }
  return Math.ceil(dense + sparse / 4);
}

function nonNegativeFinite(value: number | null | undefined): number | null {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
}

export function estimateDraftTokens(
  draft: string,
  pendingFiles: readonly DraftAttachment[]
): number {
  let total = draft.trim().length > 0 ? 4 + estimateTextTokens(draft) : 0;
  for (const file of pendingFiles) {
    total += 12 + estimateTextTokens(`${file.name} ${file.mime_type}`);
  }
  return total;
}

export function computeContextUsage(
  reportedTokens: number | null | undefined,
  draftTokens: number,
  windowSize: number | null | undefined
): ContextUsage {
  const reported = nonNegativeFinite(reportedTokens);
  const draft = nonNegativeFinite(draftTokens) ?? 0;
  const used = reported != null ? reported + draft : draft > 0 ? draft : null;
  const source: ContextUsageSource =
    reported != null ? 'reported' : used != null ? 'draft-only' : 'unknown';
  const window = nonNegativeFinite(windowSize);

  if (used == null || window == null || window <= 0) {
    return { used, pct: null, source, tier: 'unknown' };
  }

  const pct = Math.min(100, (used / window) * 100);
  const tier: ContextUsageTier = pct >= 90 ? 'critical' : pct >= 70 ? 'high' : 'normal';
  return { used, pct, source, tier };
}
