export interface UsageEventLike {
  event: string;
  model?: unknown;
  prompt_tokens?: unknown;
  completion_tokens?: unknown;
  total_tokens?: unknown;
  cached_prompt_tokens?: unknown;
  cache_creation_prompt_tokens?: unknown;
  usage_reported?: unknown;
  usage_event_id?: unknown;
}

export interface SubagentUsageRun {
  conversationId?: string;
  status: string;
  usageEvents?: readonly UsageEventLike[];
}

export interface SubagentUsageSummary {
  total: number;
  running: number;
  calls: number;
  input: number;
  output: number;
  cached: number;
  cacheCreation: number;
  missingUsage: number;
}

function tokenCount(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : 0;
}

export function isCanonicalUsageEvent(event: UsageEventLike): boolean {
  return (
    event.event === 'usage' &&
    typeof event.model === 'string' &&
    typeof event.usage_reported === 'boolean' &&
    typeof event.total_tokens === 'number'
  );
}

export function summarizeSubagentUsage(
  runs: readonly SubagentUsageRun[],
  activeConversationId: string | null | undefined
): SubagentUsageSummary {
  const summary: SubagentUsageSummary = {
    total: 0,
    running: 0,
    calls: 0,
    input: 0,
    output: 0,
    cached: 0,
    cacheCreation: 0,
    missingUsage: 0,
  };
  if (!activeConversationId) return summary;

  const seenEventIds = new Set<string>();
  for (const run of runs) {
    if (run.conversationId !== activeConversationId) continue;
    summary.total += 1;
    if (run.status === 'running') summary.running += 1;

    for (const event of run.usageEvents ?? []) {
      if (!isCanonicalUsageEvent(event)) continue;
      const eventId =
        typeof event.usage_event_id === 'string' && event.usage_event_id.length > 0
          ? event.usage_event_id
          : null;
      if (eventId && seenEventIds.has(eventId)) continue;
      if (eventId) seenEventIds.add(eventId);

      summary.calls += 1;
      if (event.usage_reported === false) {
        summary.missingUsage += 1;
        continue;
      }
      summary.input += tokenCount(event.prompt_tokens);
      summary.output += tokenCount(event.completion_tokens);
      summary.cached += tokenCount(event.cached_prompt_tokens);
      summary.cacheCreation += tokenCount(event.cache_creation_prompt_tokens);
    }
  }
  return summary;
}
