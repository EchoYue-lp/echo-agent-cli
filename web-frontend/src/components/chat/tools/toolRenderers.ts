import type { ToolExecution } from '../../../types/api';

export type ToolRendererKind =
  | 'shell'
  | 'read'
  | 'write'
  | 'search'
  | 'browser'
  | 'mcp'
  | 'task'
  | 'generic';

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

function browserDomain(value: string | undefined): string | undefined {
  if (!value) return undefined;
  try {
    return new URL(value).hostname || value;
  } catch {
    return value;
  }
}

function describeBrowser(tool: ToolExecution): ToolRenderDescriptor {
  const args = argsRecord(tool);
  const url = textArg(args, 'url');
  const observedUrl = tool.metadata?.browser_url || url;
  const domain = browserDomain(observedUrl);
  const pageTitle = tool.metadata?.browser_title;
  const target = textArg(args, 'target', 'element', 'selector', 'ref');
  const action = tool.name.replace(/^browser_/, '').replaceAll('_', ' ');
  const title =
    tool.name === 'browser_navigate'
      ? `Open ${domain || 'page'}`
      : tool.name === 'browser_snapshot'
        ? `Inspect ${domain || 'page'}`
        : `${action.charAt(0).toUpperCase()}${action.slice(1)}`;
  const detail = [
    pageTitle,
    observedUrl && domain !== observedUrl ? observedUrl : undefined,
    target,
  ]
    .filter(Boolean)
    .join(' · ');
  return {
    kind: 'browser',
    title,
    detail: detail || undefined,
    collapseSuccessfulOutput: true,
  };
}

function mcpIdentity(tool: ToolExecution): { server?: string; name: string } | undefined {
  const args = argsRecord(tool);
  const server = tool.metadata?.mcp_server || textArg(args, 'server', 'server_name');
  const metadataTool = tool.metadata?.mcp_tool;
  const namespaced = tool.name.match(/^mcp__(.+?)__(.+)$/);
  if (namespaced?.[1] && namespaced[2]) return { server: namespaced[1], name: namespaced[2] };
  if (tool.metadata?.tool_source === 'mcp' || server || metadataTool) {
    return { server, name: metadataTool || tool.name };
  }
  return undefined;
}

function mcpResultType(tool: ToolExecution): string {
  const explicit = tool.metadata?.result_type;
  if (explicit) return explicit === 'json' ? 'JSON result' : `${explicit} result`;
  const result = tool.result.trim();
  if (!result) return 'empty result';
  try {
    const value: unknown = JSON.parse(result);
    return Array.isArray(value) ? 'JSON array' : 'JSON object';
  } catch {
    return 'text result';
  }
}

function describeMcp(
  tool: ToolExecution,
  identity: { server?: string; name: string }
): ToolRenderDescriptor {
  return {
    kind: 'mcp',
    title: identity.server ? `${identity.server} · ${identity.name}` : identity.name,
    detail: tool.status === 'running' ? undefined : mcpResultType(tool),
    collapseSuccessfulOutput: true,
  };
}

function describeTask(tool: ToolExecution): ToolRenderDescriptor {
  const args = argsRecord(tool);
  const inlineTask =
    args.task && typeof args.task === 'object' ? (args.task as Record<string, unknown>) : undefined;
  const role = textArg(args, 'agent_name') || (inlineTask && textArg(inlineTask, 'agent_role'));
  const task =
    textArg(args, 'user_goal') ||
    (typeof args.task === 'string' ? args.task : undefined) ||
    (inlineTask && textArg(inlineTask, 'description'));
  const title =
    tool.name === 'agent_tool'
      ? `Subagent ${role || 'dispatch'}`
      : tool.name === 'create_complex_task'
        ? 'Start task run'
        : tool.name === 'plan_execute'
          ? role
            ? `Execute with ${role}`
            : 'Execute plan'
          : tool.name.replaceAll('_', ' ');
  return {
    kind: 'task',
    title,
    detail: task || undefined,
    collapseSuccessfulOutput: true,
  };
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
  if (tool.name.startsWith('browser_')) return describeBrowser(tool);
  if (['agent_tool', 'plan_execute', 'create_complex_task'].includes(tool.name))
    return describeTask(tool);
  const mcp = mcpIdentity(tool);
  if (mcp) return describeMcp(tool, mcp);

  const args = tool.args == null ? '' : JSON.stringify(tool.args);
  return {
    kind: 'generic',
    title: tool.name,
    detail: args || undefined,
    collapseSuccessfulOutput: false,
  };
}
