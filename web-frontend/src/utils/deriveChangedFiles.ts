import type { ChatMessage, ToolExecution } from '../types/api';

export type ChangeStatus = 'modified' | 'added' | 'deleted';

export interface ChangedFile {
  path: string;
  basename: string;
  dir: string;
  status: ChangeStatus;
  firstTouchedAt: number;
  lastTouchedAt: number;
  toolCount: number;
  /** 最近一次写入类工具调用的 args,用于非 git 场景兜底展示 */
  lastWriteArgs?: unknown;
}

interface PathStatus {
  path: string;
  status: ChangeStatus;
  /** 是否是写入类(带 content/new_string 的)工具,用于兜底 */
  writeArgs?: unknown;
}

/** 安全取 args[key] 的字符串值 */
function str(args: unknown, key: string): string {
  if (args && typeof args === 'object' && key in (args as Record<string, unknown>)) {
    const v = (args as Record<string, unknown>)[key];
    return typeof v === 'string' ? v : '';
  }
  return '';
}

/** 工具名 → 产出(路径, 状态, 写入 args)的映射表.
 *
 * NOTE: This list intentionally covers the common file-modifying tools. It does
 * not cover shell commands (sed, git apply, etc.) or arbitrary MCP tools —
 * those require backend-side git status / file watcher integration. */
const FILE_TOOL_EXTRACTORS: Record<string, (tc: ToolExecution) => PathStatus[]> = {
  create_file: (tc) => [{ path: str(tc.args, 'path'), status: 'added', writeArgs: tc.args }],
  write_file: (tc) => [{ path: str(tc.args, 'path'), status: 'modified', writeArgs: tc.args }],
  edit_file: (tc) => [{ path: str(tc.args, 'path'), status: 'modified', writeArgs: tc.args }],
  append_file: (tc) => [{ path: str(tc.args, 'path'), status: 'modified', writeArgs: tc.args }],
  update_file: (tc) => [{ path: str(tc.args, 'path'), status: 'modified', writeArgs: tc.args }],
  delete_file: (tc) => [{ path: str(tc.args, 'path'), status: 'deleted' }],
  move_file: (tc) => [
    { path: str(tc.args, 'old_path'), status: 'deleted' },
    { path: str(tc.args, 'new_path'), status: 'added', writeArgs: tc.args },
  ],
  apply_patch: (tc) => [{ path: str(tc.args, 'path'), status: 'modified', writeArgs: tc.args }],
  multi_edit: (tc) => {
    const edits =
      tc.args && typeof tc.args === 'object' && 'edits' in (tc.args as Record<string, unknown>)
        ? (tc.args as Record<string, unknown>).edits
        : null;
    if (!Array.isArray(edits)) return [];
    return edits
      .filter((e): e is Record<string, unknown> => e && typeof e === 'object')
      .map((e) => ({
        path: str(e as unknown, 'path'),
        status: 'modified' as const,
        writeArgs: tc.args,
      }));
  },
};

function splitPath(path: string): { basename: string; dir: string } {
  const norm = path.replace(/\\/g, '/');
  const idx = norm.lastIndexOf('/');
  if (idx === -1) return { basename: norm, dir: '' };
  return { basename: norm.slice(idx + 1), dir: norm.slice(0, idx) };
}

/** 从会话消息派生被改动的文件列表。跳过 success===false 的工具调用。 */
export function deriveChangedFiles(messages: ChatMessage[]): ChangedFile[] {
  const byPath = new Map<string, ChangedFile>();
  const now = Date.now();

  for (const msg of messages) {
    // 只有 assistant 消息有 toolCalls,但防御性遍历所有
    const toolCalls = msg.toolCalls ?? [];
    // 优先用消息时间戳,fallback now
    const ts = typeof msg.timestamp === 'number' && msg.timestamp > 0 ? msg.timestamp : now;

    for (const tc of toolCalls) {
      if (tc.success === false) continue; // 跳过失败工具调用
      const extractor = FILE_TOOL_EXTRACTORS[tc.name];
      if (!extractor) continue;
      const results = extractor(tc);
      for (const r of results) {
        const path = r.path.trim();
        if (!path) continue;
        const { basename, dir } = splitPath(path);
        const existing = byPath.get(path);
        if (existing) {
          existing.toolCount += 1;
          existing.lastTouchedAt = Math.max(existing.lastTouchedAt, ts);
          // status 取最近一次操作(后出现的覆盖)
          existing.status = r.status;
          if (r.writeArgs !== undefined) existing.lastWriteArgs = r.writeArgs;
        } else {
          byPath.set(path, {
            path,
            basename,
            dir,
            status: r.status,
            firstTouchedAt: ts,
            lastTouchedAt: ts,
            toolCount: 1,
            lastWriteArgs: r.writeArgs,
          });
        }
      }
    }
  }

  return Array.from(byPath.values()).sort((a, b) => b.lastTouchedAt - a.lastTouchedAt);
}
