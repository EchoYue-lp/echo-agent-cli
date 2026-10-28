import { useCallback, useEffect, useRef, useState } from 'react';
import { toolsApi, type ProductDataScope } from '../../api/endpoints';
import type { ToolInfo } from '../../generated';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { productDataScope, sameProductDataScope } from '../../lib/productDataScope';
import { ChevronDown, ChevronRight, LockKeyhole } from 'lucide-react';

const CATEGORIES = [
  ['task_collaboration', '任务与协作'],
  ['files_code', '文件与代码'],
  ['commands_runtime', '命令与运行'],
  ['search_browse', '搜索与浏览'],
  ['data_documents', '数据与文档'],
  ['memory_skills', '记忆与技能'],
  ['uncategorized', '未分类'],
] as const;

type Filter = 'all' | 'disabled' | 'required';
const categoryOf = (tool: ToolInfo) => tool.category ?? 'uncategorized';
const sourceLabel = (tool: ToolInfo) => {
  switch (tool.source.kind) {
    case 'builtin':
      return '内置';
    case 'mcp':
      return `${tool.source.plugin_id ? `插件 · ${tool.source.plugin_id} / ` : ''}MCP · ${tool.source.server}`;
    case 'unknown':
      return '来源未提供';
  }
};

export function ToolsPanel() {
  const [tools, setTools] = useState<ToolInfo[]>([]);
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState<Filter>('all');
  const [opened, setOpened] = useState<Set<string>>(new Set());
  const [details, setDetails] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const epoch = useRef(0);
  const mounted = useRef(false);
  const loadedScope = useRef<ProductDataScope | null>(null);

  const current = useCallback(
    (ticket: number, scope: ProductDataScope) =>
      mounted.current &&
      epoch.current === ticket &&
      sameProductDataScope(scope, productDataScope(useWorkspaceStore.getState().current)),
    []
  );

  const load = useCallback(async () => {
    const ticket = ++epoch.current;
    const scope = productDataScope(useWorkspaceStore.getState().current);
    setLoading(true);
    setBusy(false);
    setError(null);
    setNotice(null);
    loadedScope.current = null;
    try {
      const data = await toolsApi.list(scope);
      if (!current(ticket, scope)) return;
      loadedScope.current = scope;
      setTools(data);
    } catch (failure) {
      if (current(ticket, scope)) setError(`工具列表加载失败：${String(failure)}`);
    } finally {
      if (current(ticket, scope)) setLoading(false);
    }
  }, [current]);

  useEffect(() => {
    mounted.current = true;
    void load();
    // 订阅每次真实切换；即使React把A→B→A合并渲染，也不能继续旧批次。
    const unsubscribe = useWorkspaceStore.subscribe((next, previous) => {
      if (next.current !== previous.current) {
        setTools([]);
        void load();
      }
    });
    return () => {
      mounted.current = false;
      epoch.current += 1;
      unsubscribe();
    };
  }, [load]);

  const mutate = async (names: string[], enable: boolean, label: string) => {
    const scope = loadedScope.current;
    if (busy || loading || !scope || names.length === 0) return;
    const ticket = epoch.current;
    setBusy(true);
    setError(null);
    setNotice(null);
    const failures: string[] = [];
    let succeeded = 0;
    try {
      // 分类操作使用点击时的身份集，重新检查真实目录中的注销及required变化。
      const fresh = await toolsApi.list(scope);
      if (!current(ticket, scope)) return;
      setTools(fresh);
      for (const name of names) {
        if (!current(ticket, scope)) return;
        const tool = fresh.find((entry) => entry.name === name);
        if (!tool) {
          failures.push(`${name}：已注销，未操作`);
          continue;
        }
        if (tool.required_reason) {
          failures.push(`${name}：系统必需，保持开启`);
          continue;
        }
        try {
          const receipt = enable
            ? await toolsApi.enable(name, scope)
            : await toolsApi.disable(name, scope);
          if (!current(ticket, scope)) return;
          if (!receipt.success || receipt.name !== name) {
            failures.push(`${name}：操作回执不匹配，请重新加载`);
            continue;
          }
          succeeded += 1;
          setTools((previous) =>
            previous.map((entry) =>
              entry.name === name
                ? {
                    ...entry,
                    policy_enabled: receipt.policy_enabled,
                    enabled: receipt.effective_enabled,
                  }
                : entry
            )
          );
        } catch (failure) {
          if (!current(ticket, scope)) return;
          failures.push(`${name}：${String(failure)}`);
        }
      }
      // 原命令可能先提交policy再在publish失败；不把错误等同“完全未变更”。
      // 只重读当前权威目录，绝不自动重试变更或回滚已发生的选择。
      if (!current(ticket, scope)) return;
      try {
        const authoritative = await toolsApi.list(scope);
        if (!current(ticket, scope)) return;
        setTools(authoritative);
      } catch (failure) {
        if (!current(ticket, scope)) return;
        failures.push(`操作结束后目录刷新失败，请重新加载：${String(failure)}`);
      }
      if (current(ticket, scope)) {
        setNotice(`${label}：已处理 ${succeeded} 项可选工具；系统必需项保持开启。`);
        if (failures.length > 0) setError(failures.join('；'));
      }
    } catch (failure) {
      if (current(ticket, scope)) setError(`未取得最新工具目录，未提交变更：${String(failure)}`);
    } finally {
      if (current(ticket, scope)) setBusy(false);
    }
  };

  const toggleSet = (value: string, set: typeof setOpened) =>
    set((previous) => {
      const next = new Set(previous);
      if (next.has(value)) next.delete(value);
      else next.add(value);
      return next;
    });
  const term = query.trim().toLocaleLowerCase();
  const matches = (tool: ToolInfo) =>
    (!term || `${tool.name} ${tool.description}`.toLocaleLowerCase().includes(term)) &&
    (filter === 'all' ||
      (filter === 'disabled' ? !tool.policy_enabled : tool.required_reason !== null));
  const shownCount = tools.filter(matches).length;

  return (
    <div className="p-3 space-y-3">
      <h3 className="text-sm font-semibold">工具 ({tools.length})</h3>
      <p className="text-xs" style={{ color: 'var(--text-secondary)' }}>
        选择 Agent 可使用的工具，对后续运行生效；当前运行保持原快照。当前选择保存在本次应用内。
      </p>
      <input
        type="search"
        aria-label="搜索工具名称或说明"
        placeholder="搜索工具名称或说明"
        value={query}
        onChange={(event) => setQuery(event.target.value)}
        className="w-full rounded-lg border px-3 py-2 text-sm"
        style={{
          background: 'var(--bg-primary)',
          borderColor: 'var(--border-primary)',
        }}
      />
      <div className="flex flex-wrap gap-2" aria-label="工具筛选">
        {(
          [
            ['all', '全部'],
            ['disabled', '已关闭'],
            ['required', '系统必需'],
          ] as const
        ).map(([value, label]) => (
          <button
            key={value}
            type="button"
            aria-pressed={filter === value}
            onClick={() => setFilter(value)}
            className="rounded border px-2 py-1 text-xs"
          >
            {label}
          </button>
        ))}
        <button type="button" disabled={busy || loading} onClick={() => void load()}>
          重新加载
        </button>
      </div>
      {loading && <p role="status">正在加载工具…</p>}
      {error && (
        <div role="alert" className="text-xs">
          {error}
        </div>
      )}
      {notice && (
        <div role="status" className="text-xs">
          {notice}
        </div>
      )}
      {!loading && shownCount === 0 && <p>没有匹配的工具</p>}
      {CATEGORIES.map(([category, label]) => {
        const all = tools.filter((tool) => categoryOf(tool) === category);
        const visible = all.filter(matches);
        if (visible.length === 0) return null;
        const optional = all.filter((tool) => !tool.required_reason);
        const enabledCount = optional.filter((tool) => tool.policy_enabled).length;
        const requiredCount = all.length - optional.length;
        const mixed = enabledCount > 0 && enabledCount < optional.length;
        const expanded = term.length > 0 || filter !== 'all' || opened.has(category);
        return (
          <section
            key={category}
            className="rounded-lg border"
            style={{ borderColor: 'var(--border-primary)' }}
          >
            <div className="flex items-center gap-2 px-3 py-2">
              <button
                type="button"
                aria-expanded={expanded}
                aria-controls={`tools-${category}`}
                onClick={() => toggleSet(category, setOpened)}
                className="flex flex-1 items-center gap-2 text-sm text-left"
              >
                {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                {label}
                <span className="text-xs">
                  可选已启用 {enabledCount}/{optional.length} · 系统必需 {requiredCount}
                </span>
              </button>
              <input
                type="checkbox"
                aria-label={`切换${label}全部可选工具`}
                aria-checked={
                  mixed ? 'mixed' : enabledCount === optional.length && optional.length > 0
                }
                checked={enabledCount === optional.length && optional.length > 0}
                ref={(element) => {
                  if (element) element.indeterminate = mixed;
                }}
                disabled={busy || loading || optional.length === 0}
                onChange={(event) =>
                  void mutate(
                    optional.map((tool) => tool.name),
                    event.target.checked,
                    label
                  )
                }
              />
            </div>
            {expanded && (
              <div id={`tools-${category}`} className="border-t px-3 py-2 space-y-2">
                <p className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
                  分类开关作用于该类全部可选工具，包含筛选未显示的项；系统必需项不受影响。
                </p>
                {visible.map((tool) => (
                  <div
                    key={tool.name}
                    className="rounded border p-2"
                    style={{ borderColor: 'var(--border-primary)' }}
                  >
                    <div className="flex items-center gap-2">
                      <button
                        type="button"
                        aria-expanded={details.has(tool.name)}
                        aria-label={`查看${tool.name}详情`}
                        aria-describedby={
                          tool.required_reason
                            ? `tool-required-${encodeURIComponent(tool.name)}`
                            : undefined
                        }
                        onClick={() => toggleSet(tool.name, setDetails)}
                        className="flex-1 text-left font-mono text-xs"
                      >
                        {tool.name}
                      </button>
                      <span className="text-xs">{sourceLabel(tool)}</span>
                      {tool.required_reason && <LockKeyhole size={14} aria-label="系统必需" />}
                      <input
                        type="checkbox"
                        role="switch"
                        aria-label={`${tool.name}启用选择`}
                        checked={tool.policy_enabled}
                        disabled={Boolean(tool.required_reason) || busy || loading}
                        onChange={(event) =>
                          void mutate([tool.name], event.target.checked, tool.name)
                        }
                      />
                    </div>
                    {tool.required_reason && (
                      <p id={`tool-required-${encodeURIComponent(tool.name)}`} className="text-xs">
                        系统必需：{tool.required_reason}
                      </p>
                    )}
                    {tool.policy_enabled && !tool.enabled && (
                      <p className="text-xs">已允许；当前运行能力或工具范围暂不可用。</p>
                    )}
                    {details.has(tool.name) && (
                      <div className="mt-2 text-xs">
                        <p>{tool.description}</p>
                        {tool.parameters && (
                          <pre className="mt-2 max-h-40 overflow-auto">
                            {JSON.stringify(tool.parameters, null, 2)}
                          </pre>
                        )}
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}
          </section>
        );
      })}
    </div>
  );
}
