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
}

export function isSubagentDispatchTool(name: string): boolean {
  return name === 'agent_tool' || name === 'plan_execute';
}

function displayToolName(name: string): string {
  const labels: Record<string, string> = {
    shell: 'Shell',
    read_file: 'Read',
    edit_file: 'Edit',
    write_file: 'Write',
    create_file: 'Create',
    grep: 'Grep',
    glob: 'Glob',
    code_search: 'Code Search',
    search_text: 'Search Text',
    browser_navigate: 'Navigate',
    browser_snapshot: 'Snapshot',
    agent_tool: 'Agent Tool',
    plan_execute: 'Plan Execute',
    plan_create: 'plan_create',
    plan_patch: 'Plan Patch',
    task_list: 'Task List',
    create_complex_task: 'Create Complex Task',
    check_run_status: 'Check Run Status',
    cancel_run: 'Cancel Run',
  };
  const known = labels[name];
  if (known) return known;
  const words = name.replace(/^browser_/, '').replaceAll('_', ' ');
  return `${words.charAt(0).toUpperCase()}${words.slice(1)}`;
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

function genericArgSummary(args: Record<string, unknown>): string | undefined {
  const path = textArg(args, 'path', 'file_path', 'file', 'directory', 'root', 'cwd');
  const query = textArg(args, 'query', 'q', 'pattern', 'glob', 'symbol', 'search', 'term');
  if (query && path) return `“${query}” · in ${path}`;
  if (query) return `“${query}”`;
  if (path) return path;
  return textArg(args, 'command', 'url', 'target', 'name', 'task_id', 'run_id', 'id');
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
  const action = displayToolName(tool.name);
  const title =
    tool.name === 'browser_navigate'
      ? `${action} ${domain || 'page'}`
      : tool.name === 'browser_snapshot'
        ? `${action} ${domain || 'page'}`
        : action;
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
  const detail = [
    identity.server,
    genericArgSummary(argsRecord(tool)),
    tool.status === 'running' ? undefined : mcpResultType(tool),
  ]
    .filter(Boolean)
    .join(' · ');
  return {
    kind: 'mcp',
    title: displayToolName(identity.name),
    detail: detail || undefined,
  };
}

function describeTask(tool: ToolExecution): ToolRenderDescriptor {
  const args = argsRecord(tool);
  const inlineTask =
    args.task && typeof args.task === 'object' ? (args.task as Record<string, unknown>) : undefined;
  const role = textArg(args, 'agent_name') || (inlineTask && textArg(inlineTask, 'agent_role'));
  const task =
    textArg(args, 'title') ||
    textArg(args, 'user_goal') ||
    (typeof args.task === 'string' ? args.task : undefined) ||
    (inlineTask && textArg(inlineTask, 'description')) ||
    textArg(args, 'description', 'task_id', 'run_id');
  const title =
    tool.name === 'agent_tool' && role
      ? `${displayToolName(tool.name)} ${role}`
      : tool.name === 'plan_execute' && role
        ? `${displayToolName(tool.name)} ${role}`
        : displayToolName(tool.name);
  return {
    kind: 'task',
    title,
    detail: task || undefined,
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
  return { kind: 'read', title: `${displayToolName(tool.name)} ${path}`, detail: range };
}

function describeWrite(tool: ToolExecution): ToolRenderDescriptor {
  const args = argsRecord(tool);
  const path = textArg(args, 'path', 'file_path') || 'file';
  const action = displayToolName(tool.name);
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
    title: `${displayToolName(tool.name)} “${query}”`,
    detail:
      [path !== '.' ? `in ${path}` : undefined, filter, count].filter(Boolean).join(' · ') ||
      undefined,
  };
}

export function describeToolExecution(tool: ToolExecution): ToolRenderDescriptor {
  if (tool.name === 'shell') {
    const command = textArg(argsRecord(tool), 'command') || 'shell';
    return { kind: 'shell', title: `${displayToolName(tool.name)} ${command}` };
  }
  if (tool.name === 'read_file') return describeRead(tool);
  if (['edit_file', 'write_file', 'create_file'].includes(tool.name)) return describeWrite(tool);
  if (['grep', 'glob', 'code_search', 'search_text'].includes(tool.name))
    return describeSearch(tool);
  if (tool.name.startsWith('browser_')) return describeBrowser(tool);
  if (
    [
      'agent_tool',
      'plan_execute',
      'plan_create',
      'plan_patch',
      'task_list',
      'create_complex_task',
      'check_run_status',
      'cancel_run',
    ].includes(tool.name)
  )
    return describeTask(tool);
  const mcp = mcpIdentity(tool);
  if (mcp) return describeMcp(tool, mcp);

  return {
    kind: 'generic',
    title: displayToolName(tool.name),
    detail: genericArgSummary(argsRecord(tool)),
  };
}
