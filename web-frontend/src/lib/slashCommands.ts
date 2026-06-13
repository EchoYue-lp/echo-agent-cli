export interface SlashCommandArg {
  name: string;
  description: string;
  required: boolean;
}

export interface SlashCommand {
  name: string;
  aliases: string[];
  description: string;
  category: string;
  icon?: string;
  args?: SlashCommandArg[];
  action?: 'send' | 'api';
}

export const SLASH_COMMANDS: SlashCommand[] = [
  // Session
  {
    name: '/reset',
    aliases: [],
    description: 'Reset conversation',
    category: 'Session',
    action: 'send',
  },
  {
    name: '/history',
    aliases: [],
    description: 'View conversation history',
    category: 'Session',
    action: 'send',
  },
  {
    name: '/stats',
    aliases: [],
    description: 'Show session statistics',
    category: 'Session',
    action: 'send',
  },

  // Context
  {
    name: '/mode',
    aliases: [],
    description: 'Switch agent mode (general/coding/research/data/writing)',
    category: 'Context',
    action: 'send',
  },
  {
    name: '/model',
    aliases: [],
    description: 'Switch LLM model',
    category: 'Context',
    action: 'send',
  },
  {
    name: '/compress',
    aliases: ['/compact'],
    description: 'Compress context',
    category: 'Context',
    action: 'send',
  },
  {
    name: '/memory',
    aliases: [],
    description: 'View/manage memory',
    category: 'Context',
    action: 'send',
  },
  {
    name: '/remember',
    aliases: [],
    description: 'Save something to memory',
    category: 'Context',
    action: 'send',
  },

  // Security
  {
    name: '/permission',
    aliases: ['/perm'],
    description: 'Set permission mode (default/plan/auto-edit/full-auto)',
    category: 'Security',
    action: 'send',
  },

  // Coding
  {
    name: '/plan',
    aliases: [],
    description: 'Enter plan mode (read-only analysis)',
    category: 'Coding',
    action: 'send',
  },
  {
    name: '/tasks',
    aliases: [],
    description: 'Manage background tasks',
    category: 'Coding',
    action: 'send',
  },
  { name: '/test', aliases: [], description: 'Run tests', category: 'Coding', action: 'send' },
  {
    name: '/code-review',
    aliases: [],
    description: 'Review code changes',
    category: 'Coding',
    action: 'send',
  },
  {
    name: '/diff',
    aliases: [],
    description: 'Show file diff or git diff',
    category: 'Coding',
    action: 'send',
  },

  // Git
  {
    name: '/git',
    aliases: [],
    description: 'Git operations (status/log/diff/commit/blame)',
    category: 'Git',
    action: 'send',
  },

  // Pipeline
  {
    name: '/pipeline',
    aliases: [],
    description: 'Run a pipeline (research/writing/data)',
    category: 'Pipeline',
    action: 'send',
  },

  // Scheduling
  {
    name: '/cron',
    aliases: ['/schedule'],
    description: 'Manage scheduled tasks',
    category: 'Scheduling',
    action: 'send',
  },

  // Info
  {
    name: '/tools',
    aliases: [],
    description: 'List available tools',
    category: 'Info',
    action: 'send',
  },
  { name: '/help', aliases: [], description: 'Show help', category: 'Info', action: 'send' },
];

/** Category display order and icons */
export const CATEGORY_META: Record<string, { icon: string; order: number }> = {
  Session: { icon: '🔄', order: 0 },
  Context: { icon: '🧠', order: 1 },
  Security: { icon: '🔒', order: 2 },
  Coding: { icon: '💻', order: 3 },
  Git: { icon: '📦', order: 4 },
  Pipeline: { icon: '⚙️', order: 5 },
  Scheduling: { icon: '⏰', order: 6 },
  Memory: { icon: '💾', order: 7 },
  Info: { icon: 'ℹ️', order: 8 },
};

/**
 * Filter and sort slash commands based on user query.
 * query should include the leading `/`.
 */
export function filterCommands(query: string): SlashCommand[] {
  const q = query.toLowerCase().trim();
  if (!q.startsWith('/')) return [];

  const searchTerm = q.slice(1); // remove leading /

  return SLASH_COMMANDS.filter((cmd) => {
    const nameWithoutSlash = cmd.name.slice(1);
    if (nameWithoutSlash.startsWith(searchTerm)) return true;
    if (cmd.aliases.some((a) => a.slice(1).startsWith(searchTerm))) return true;
    return false;
  }).sort((a, b) => {
    // Sort by category order, then alphabetically
    const catA = CATEGORY_META[a.category]?.order ?? 99;
    const catB = CATEGORY_META[b.category]?.order ?? 99;
    if (catA !== catB) return catA - catB;
    return a.name.localeCompare(b.name);
  });
}

/**
 * Group commands by category for display.
 */
export function groupByCategory(commands: SlashCommand[]): Map<string, SlashCommand[]> {
  const groups = new Map<string, SlashCommand[]>();
  for (const cmd of commands) {
    const existing = groups.get(cmd.category);
    if (existing) {
      existing.push(cmd);
    } else {
      groups.set(cmd.category, [cmd]);
    }
  }
  return groups;
}
