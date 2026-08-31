import type { SubagentRunState } from '../stores/subagentRunStore';

export interface SubagentOutcomePresentation {
  text: string;
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

export function subagentOutcomePresentation(run: SubagentRunState): SubagentOutcomePresentation {
  const terminalOutput = stripTerminalContract(run.finalOutput ?? '');
  const summary = run.outcome?.summary.trim() || '';
  return { text: terminalOutput || summary };
}
