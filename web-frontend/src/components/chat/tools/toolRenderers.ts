import type { ToolExecution } from '../../../types/api';

export type ToolRendererKind = 'shell' | 'read' | 'write' | 'search' | 'generic';

export interface ToolRenderDescriptor {
  kind: ToolRendererKind;
  title: string;
  detail?: string;
  collapseSuccessfulOutput: boolean;
}

function argsRecord(tool: ToolExecution): Record<string, unknown> {
  return tool.args && typeof tool.args === 'object' ? (tool.args as Record<string, unknown>) : {};
}

function textArg(args: Record<string, unknown>, ...keys: string[]): string | undefined {
  for (const key of keys) {
    const value = args[key];
    if (typeof value === 'string' && value.length > 0) return value;
  }
  return undefined;
}

function numberArg(args: Record<string, unknown>, ...keys: string[]): number | undefined {
  for (const key of keys) {
    const value = args[key];
    if (typeof value === 'number' && Number.isFinite(value)) return value;
  }
  return undefined;
}

function resultCount(result: string): string | undefined {
  const match = result.match(/(?:showing\s+\d+\s+of\s+)?(\d+)\s+(?:matches|results|files?)\b/i);
  return match?.[1] ? `${match[1]} matches` : undefined;
}

function describeRead(tool: ToolExecution): ToolRenderDescriptor {
  const args = argsRecord(tool);
  const path = textArg(args, 'path', 'file_path') || 'file';
  const offset = numberArg(args, 'offset', 'start_line') ?? 1;
  const limit = numberArg(args, 'limit', 'line_count');
  const range =
    limit == null
      ? `from line ${offset}`
      : limit < 0
        ? `preview from line ${offset}`
        : `lines ${offset}-${Math.max(offset, offset + limit - 1)}`;
  return { kind: 'read', title: path, detail: range, collapseSuccessfulOutput: true };
}

function describeWrite(tool: ToolExecution): ToolRenderDescriptor {
  const args = argsRecord(tool);
  const path = textArg(args, 'path', 'file_path') || 'file';
  const action =
    tool.name === 'edit_file' ? 'Edit' : tool.name === 'create_file' ? 'Create' : 'Write';
  const details: string[] = [];
  if (args.dry_run === true) details.push('dry run');
  const content = textArg(args, 'content', 'new_content');
  if (content != null) {
    const lineCount = content.length === 0 ? 0 : content.split('\n').length;
    details.push(`${lineCount} lines`);
  }
  const originalSize = tool.metadata?.original_size;
  const updatedSize = tool.metadata?.updated_size;
  if (originalSize != null && updatedSize != null)
    details.push(`${originalSize} → ${updatedSize} bytes`);
  return {
    kind: 'write',
    title: `${action} ${path}`,
    detail: details.join(' · ') || undefined,
    collapseSuccessfulOutput: true,
  };
}

function describeSearch(tool: ToolExecution): ToolRenderDescriptor {
  const args = argsRecord(tool);
  const query = textArg(args, 'query', 'pattern', 'symbol') || 'query';
  const path = textArg(args, 'path') || '.';
  const filter = textArg(args, 'glob', 'file_type');
  const count = resultCount(tool.result || tool.stdout);
  return {
    kind: 'search',
    title: `${tool.name === 'glob' ? 'Find' : 'Search'} “${query}”`,
    detail:
      [path !== '.' ? `in ${path}` : undefined, filter, count].filter(Boolean).join(' · ') ||
      undefined,
    collapseSuccessfulOutput: true,
  };
}

export function describeToolExecution(tool: ToolExecution): ToolRenderDescriptor {
  if (tool.name === 'shell') {
    const command = textArg(argsRecord(tool), 'command') || 'shell';
    return { kind: 'shell', title: command, collapseSuccessfulOutput: false };
  }
  if (tool.name === 'read_file') return describeRead(tool);
  if (['edit_file', 'write_file', 'create_file'].includes(tool.name)) return describeWrite(tool);
  if (['grep', 'glob', 'code_search', 'search_text'].includes(tool.name))
    return describeSearch(tool);

  const args = tool.args == null ? '' : JSON.stringify(tool.args);
  return {
    kind: 'generic',
    title: tool.name,
    detail: args || undefined,
    collapseSuccessfulOutput: false,
  };
}
