import type { SubagentRunState } from '../stores/subagentRunStore';

export interface SubagentResultPresentation {
  text: string;
  promotedThinking?: string;
}

function stripTerminalContract(output: string): string {
  const lines = output.replace(/\r\n/g, '\n').split('\n');
  for (let index = lines.length - 1; index >= 0; index -= 1) {
    if (lines[index]?.trim() === '## Result') {
      const envelope = lines
        .slice(index + 1)
        .join('\n')
        .trim();
      const fencedJson = envelope.match(/^```json\s*\n([\s\S]*?)\n```\s*$/i);
      if (!fencedJson?.[1]) continue;
      try {
        const parsed = JSON.parse(fencedJson[1]) as Record<string, unknown>;
        if (typeof parsed.contract_version === 'number' && typeof parsed.summary === 'string') {
          return lines.slice(0, index).join('\n').trim();
        }
      } catch {
        // A user-facing section named "Result" is not the internal contract.
      }
    }
  }
  return output.trim();
}

function thinkingSegments(run: SubagentRunState): string[] {
  const segments: string[] = [];
  const current: string[] = [];
  const flush = () => {
    const content = current.join('').trim();
    if (content) segments.push(content);
    current.length = 0;
  };

  for (const event of run.events) {
    if (event.event === 'thinking_delta') {
      if (typeof event.content === 'string') current.push(event.content);
      continue;
    }
    if (event.event === 'thinking_ended' || event.event === 'tool_started') {
      flush();
    }
  }
  flush();
  return segments;
}

function refersToEarlierContent(value: string): boolean {
  const normalized = value.trim().toLocaleLowerCase().replace(/\s+/g, ' ');
  if (!normalized || normalized.length > 600) return false;
  return /见(?:上方|上文|前文)|如上|详见.*(?:上方|上文|前文)|see (?:the )?(?:analysis|result|report|details?).*above|see full report above|as (?:shown|described|detailed) above/.test(
    normalized
  );
}

export function subagentResultPresentation(run: SubagentRunState): SubagentResultPresentation {
  const terminalOutput = stripTerminalContract(run.finalOutput ?? '');
  const liveText = run.status === 'running' ? run.streamedText?.trim() || '' : '';
  const summary = run.result?.summary.trim() || '';
  const preferred = terminalOutput || liveText || summary;

  if (run.status !== 'running') {
    const segments = thinkingSegments(run);
    const lastThinking = segments.at(-1) ?? '';
    const missingTerminalResult = !preferred;
    const referentialTerminalResult = refersToEarlierContent(preferred);
    if (
      lastThinking.length >= 120 &&
      (missingTerminalResult ||
        (referentialTerminalResult && lastThinking.length > preferred.length))
    ) {
      return { text: lastThinking, promotedThinking: lastThinking };
    }
  }

  return { text: preferred };
}

export function withoutPromotedThinking<T extends { type: string; content?: string }>(
  steps: readonly T[],
  promotedThinking?: string
): T[] {
  if (!promotedThinking) return [...steps];
  let promotedIndex = -1;
  for (let index = steps.length - 1; index >= 0; index -= 1) {
    const step = steps[index];
    if (step?.type === 'thinking' && step.content?.trim() === promotedThinking) {
      promotedIndex = index;
      break;
    }
  }
  return steps.filter((_, index) => index !== promotedIndex);
}
