export type GuiSlashTarget =
  | 'clear'
  | 'goal'
  | 'side'
  | 'tasks'
  | 'analysis'
  | 'research'
  | 'browser'
  | 'files'
  | 'workflows'
  | 'extract';

export interface SlashCommand {
  name: string;
  aliases: string[];
  description: string;
  category: string;
  target: GuiSlashTarget;
  acceptsArguments?: boolean;
}

export const SLASH_COMMANDS: SlashCommand[] = [
  {
    name: '/goal',
    aliases: [],
    description: '创建或管理当前目标，可附目标文本',
    category: 'Session',
    target: 'goal',
    acceptsArguments: true,
  },
  {
    name: '/side',
    aliases: [],
    description: '新建支线对话，可附要处理的问题',
    category: 'Session',
    target: 'side',
    acceptsArguments: true,
  },
  {
    name: '/clear',
    aliases: ['/cls'],
    description: 'Clear current chat',
    category: 'Session',
    target: 'clear',
  },
  {
    name: '/tasks',
    aliases: [],
    description: 'Open task runtime',
    category: 'Workspace',
    target: 'tasks',
  },
  {
    name: '/analysis',
    aliases: [],
    description: 'Open data analysis',
    category: 'Workspace',
    target: 'analysis',
  },
  {
    name: '/research',
    aliases: ['/papers'],
    description: 'Open research workbench',
    category: 'Workspace',
    target: 'research',
  },
  {
    name: '/browser',
    aliases: [],
    description: 'Open browser workspace',
    category: 'Workspace',
    target: 'browser',
  },
  {
    name: '/files',
    aliases: [],
    description: 'Open workspace files',
    category: 'Workspace',
    target: 'files',
  },
  {
    name: '/workflow',
    aliases: [],
    description: 'Open workflow management',
    category: 'Workspace',
    target: 'workflows',
  },
  {
    name: '/extract',
    aliases: [],
    description: 'Open structured extraction',
    category: 'Workspace',
    target: 'extract',
  },
];

export const CATEGORY_META: Record<string, { order: number }> = {
  Session: { order: 0 },
  Workspace: { order: 1 },
};

export type GuiSlashHandlers = Record<GuiSlashTarget, (argument: string) => void | Promise<void>>;

/** 仅命令 token 忽略大小写；参数保留原文，不把 /goalfoo 当作 /goal。 */
export function parseGuiSlashCommand(input: string) {
  const match = /^(\/\S+)(?:\s+([\s\S]*))?$/.exec(input.trim());
  if (!match) return null;
  const token = match[1]?.toLowerCase();
  const argument = match[2]?.trim() ?? '';
  const command = SLASH_COMMANDS.find(
    (candidate) => candidate.name === token || candidate.aliases.includes(token ?? '')
  );
  return command && (!argument || command.acceptsArguments) ? { command, argument } : null;
}

/** Dispatch an exact command exposed by the GUI palette. */
export async function dispatchGuiSlashCommand(
  input: string,
  handlers: GuiSlashHandlers
): Promise<boolean> {
  const parsed = parseGuiSlashCommand(input);
  if (!parsed) return false;
  await handlers[parsed.command.target](parsed.argument);
  return true;
}

/** Filter and sort slash commands based on a leading-slash query. */
export function filterCommands(query: string): SlashCommand[] {
  const normalized = query.toLowerCase().trim();
  if (!normalized.startsWith('/')) return [];
  const searchTerm = normalized.slice(1);
  return SLASH_COMMANDS.filter((command) => {
    const name = command.name.slice(1);
    if (name.startsWith(searchTerm)) return true;
    return command.aliases.some((alias) => alias.slice(1).startsWith(searchTerm));
  }).sort((left, right) => {
    const leftOrder = CATEGORY_META[left.category]?.order ?? 99;
    const rightOrder = CATEGORY_META[right.category]?.order ?? 99;
    if (leftOrder !== rightOrder) return leftOrder - rightOrder;
    return left.name.localeCompare(right.name);
  });
}

export function groupByCategory(commands: SlashCommand[]): Map<string, SlashCommand[]> {
  const groups = new Map<string, SlashCommand[]>();
  for (const command of commands) {
    const existing = groups.get(command.category);
    if (existing) {
      existing.push(command);
    } else {
      groups.set(command.category, [command]);
    }
  }
  return groups;
}
