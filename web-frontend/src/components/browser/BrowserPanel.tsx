import { useCallback, useEffect, useMemo, useState } from 'react';
import { Activity, Chrome, Globe2, Play } from 'lucide-react';
import { useBrowserEvents } from '../../hooks/useBrowserEvents';
import { useConversationStore } from '../../stores/conversationStore';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { useBrowserStore } from '../../stores/browserStore';
import { BrowserStatus } from './BrowserStatus';
import { BrowserTabs } from './BrowserTabs';
import { BrowserToolbar } from './BrowserToolbar';
import { BrowserViewport } from './BrowserViewport';
import { ChromeSetupDialog } from './ChromeSetupDialog';
import { viewAddress, viewAddressKey, workspaceIdForView } from '../../lib/viewAddress';
import { extensionRequestScope } from '../../api/endpoints';
import type { BrowserCommand } from '../../generated';

type BrowserToolAction =
  | 'status'
  | 'snapshot'
  | 'click_target'
  | 'fill'
  | 'type_at'
  | 'console'
  | 'network'
  | 'dom_inspect'
  | 'performance_trace';

export interface BrowserToolValues {
  target: string;
  text: string;
  element: string;
  filename: string;
  button: string;
  effect: string;
  x: string;
  y: string;
  level: string;
  method: string;
  status: string;
  contains: string;
  maxDepth: string;
  traceAction: string;
  path: string;
  doubleClick: boolean;
  submit: boolean;
  slowly: boolean;
}

const INITIAL_TOOL_VALUES: BrowserToolValues = {
  target: '',
  text: '',
  element: '',
  filename: '',
  button: '',
  effect: 'none',
  x: '0',
  y: '0',
  level: '',
  method: '',
  status: '',
  contains: '',
  maxDepth: '',
  traceAction: 'start',
  path: '',
  doubleClick: false,
  submit: false,
  slowly: false,
};

export function BrowserPanel() {
  useBrowserEvents();
  const [chromeSetupOpen, setChromeSetupOpen] = useState(false);
  const [toolsOpen, setToolsOpen] = useState(false);
  const conversationId = useConversationStore((state) => state.activeId);
  const workspace = useWorkspaceStore((state) => state.current);
  const requestScope = useMemo(() => extensionRequestScope(workspace), [workspace]);
  const workspaceId = workspaceIdForView(workspace?.id);
  const browserConversationId = conversationId ?? `ui-preview:${workspaceId}`;
  const browserAddressKey = viewAddressKey(viewAddress(workspaceId, browserConversationId));
  const view = useBrowserStore((state) => state.views[browserAddressKey]);
  const store = useBrowserStore();
  const refreshChromeStatus = useBrowserStore((state) => state.refreshChromeStatus);
  const refreshFrame = useBrowserStore((state) => state.refreshFrame);
  const viewStatus = view?.session.status;
  const handleChromeConnectionChange = useCallback((connected: boolean) => {
    useBrowserStore.setState({ chromeConnected: connected });
  }, []);
  useEffect(() => {
    void refreshChromeStatus(requestScope);
  }, [refreshChromeStatus, requestScope]);
  useEffect(() => {
    if (viewStatus !== 'ready') return;
    const timer = window.setInterval(
      () => void refreshFrame(requestScope, browserConversationId),
      1500
    );
    return () => window.clearInterval(timer);
  }, [browserConversationId, refreshFrame, requestScope, viewStatus]);
  const activeTab =
    view?.session.tabs.find((tab) => tab.id === view.activeTabId) ?? view?.session.tabs[0];
  const busy =
    view?.session.status === 'navigating' ||
    view?.session.status === 'acting' ||
    view?.session.status === 'starting';

  const call = (fn: (scope: typeof requestScope, conversationId: string) => Promise<void>) => {
    void fn(requestScope, browserConversationId);
  };
  return (
    <>
      <div className="flex h-full min-h-0 flex-col">
        <BrowserToolbar
          url={activeTab?.url ?? ''}
          busy={Boolean(busy)}
          onNavigate={(url) => void store.navigate(requestScope, browserConversationId, url)}
          onBack={() => call(store.back)}
          onReload={() => call(store.reload)}
          onStop={() => void store.stop(requestScope, browserConversationId)}
          onRefreshFrame={() => call(store.refreshFrame)}
          onNewTab={() => call(store.newTab)}
          toolsOpen={toolsOpen}
          onToggleTools={() => setToolsOpen((open) => !open)}
          backend={view?.session.backend ?? 'managed'}
          chromeConnected={store.chromeConnected}
          onBackendChange={(backend) => {
            void store.setBackend(requestScope, browserConversationId, backend).then((result) => {
              if (result.status === 'failed' && backend === 'chrome') setChromeSetupOpen(true);
            });
          }}
          onChromeSetup={() => setChromeSetupOpen(true)}
        />
        {toolsOpen && (
          <BrowserToolShelf
            busy={Boolean(busy)}
            developerMode={Boolean(view?.session.developer_mode)}
            onExecute={(command) => store.execute(requestScope, browserConversationId, command)}
          />
        )}
        <BrowserTabs
          tabs={view?.session.tabs ?? []}
          activeTabId={view?.activeTabId ?? null}
          onSelect={(index) => void store.selectTab(requestScope, browserConversationId, index)}
          onClose={(index) => void store.closeTab(requestScope, browserConversationId, index)}
        />
        <BrowserViewport
          frame={view?.frame}
          busy={Boolean(busy)}
          clickable={Boolean(view && view.session.backend !== 'chrome')}
          scrollable={Boolean(view)}
          onClickAt={(x, y) => void store.clickAt(requestScope, browserConversationId, x, y)}
          onScroll={(deltaX, deltaY) =>
            void store.scroll(requestScope, browserConversationId, deltaX, deltaY)
          }
        />
        {(busy ||
          view?.session.status === 'waiting_confirmation' ||
          view?.error ||
          store.commandErrors[browserAddressKey] ||
          store.commandPending[browserAddressKey] ||
          store.commandReceipts[browserAddressKey] ||
          Boolean(view?.diagnostics.length) ||
          view?.session.backend === 'chrome') && (
          <footer className="flex h-7 shrink-0 items-center justify-between gap-3 border-t border-[var(--border-primary)] px-2.5">
            <BrowserStatus
              status={view?.session.status}
              error={view?.error ?? store.commandErrors[browserAddressKey]}
            />
            <div className="flex min-w-0 items-center gap-2 text-[10px] text-[var(--text-tertiary)]">
              {store.commandPending[browserAddressKey] && (
                <span className="min-w-0 flex-1 truncate text-[var(--color-warning)]">
                  {store.commandPending[browserAddressKey]}
                </span>
              )}
              {store.commandReceipts[browserAddressKey] && (
                <span className="min-w-0 flex-1 truncate">
                  {store.commandReceipts[browserAddressKey]}
                </span>
              )}
              {Boolean(view?.diagnostics.length) && (
                <span
                  className="flex shrink-0 items-center gap-1"
                  title={`诊断记录 ${view?.diagnostics.length ?? 0}`}
                >
                  <Activity size={11} />
                  {view?.diagnostics.length}
                </span>
              )}
              {view?.session.backend === 'chrome' && (
                <span className="flex shrink-0 items-center gap-1" title="使用已授权 Chrome 标签页">
                  <Chrome size={11} />
                  Chrome
                </span>
              )}
              <span className="flex min-w-0 items-center gap-1">
                <Globe2 size={11} />
                <span className="truncate">{activeTab?.url ?? 'about:blank'}</span>
              </span>
            </div>
          </footer>
        )}
      </div>
      {chromeSetupOpen && (
        <ChromeSetupDialog
          requestScope={requestScope}
          onClose={() => setChromeSetupOpen(false)}
          onConnectionChange={handleChromeConnectionChange}
          onUseChrome={async () => {
            const result = await store.setBackend(requestScope, browserConversationId, 'chrome');
            if (result.status === 'settled') setChromeSetupOpen(false);
            return result;
          }}
        />
      )}
    </>
  );
}

export function BrowserToolShelf({
  busy,
  developerMode,
  onExecute,
}: {
  busy: boolean;
  developerMode: boolean;
  onExecute: (command: BrowserCommand) => Promise<unknown>;
}) {
  const [action, setAction] = useState<BrowserToolAction>('status');
  const [values, setValues] = useState(INITIAL_TOOL_VALUES);
  const [executing, setExecuting] = useState(false);
  const command = browserToolCommand(action, values);
  const canExecute = browserToolCommandReady(action, values);
  const update = (patch: Partial<BrowserToolValues>) =>
    setValues((current) => ({ ...current, ...patch }));
  const run = async (nextCommand: BrowserCommand) => {
    setExecuting(true);
    try {
      await onExecute(nextCommand);
    } finally {
      setExecuting(false);
    }
  };

  return (
    <form
      className="flex min-h-10 shrink-0 flex-wrap items-center gap-1.5 border-b border-[var(--border-primary)] bg-[var(--bg-sidebar)] px-2 py-1.5"
      onSubmit={(event) => {
        event.preventDefault();
        if (canExecute) void run(command);
      }}
    >
      <select
        value={action}
        onChange={(event) => setAction(event.target.value as BrowserToolAction)}
        className={toolSelectClass}
        aria-label="浏览器工具"
      >
        <option value="status">状态</option>
        <option value="snapshot">页面快照</option>
        <option value="click_target">目标点击</option>
        <option value="fill">填写</option>
        <option value="type_at">坐标输入</option>
        <option value="console">控制台</option>
        <option value="network">网络</option>
        <option value="dom_inspect">DOM 检查</option>
        <option value="performance_trace">性能跟踪</option>
      </select>
      <BrowserToolFields action={action} values={values} update={update} />
      <label className={toolToggleClass} title="开发者模式">
        <input
          type="checkbox"
          checked={developerMode}
          disabled={busy || executing}
          onChange={(event) =>
            void run({ action: 'developer_mode', enabled: event.target.checked })
          }
        />
        开发者
      </label>
      <button
        type="submit"
        className={toolIconButtonClass}
        disabled={busy || executing || !canExecute}
        title="执行"
      >
        <Play size={12} />
      </button>
    </form>
  );
}

function BrowserToolFields({
  action,
  values,
  update,
}: {
  action: BrowserToolAction;
  values: BrowserToolValues;
  update: (patch: Partial<BrowserToolValues>) => void;
}) {
  if (action === 'status') return null;
  if (action === 'snapshot') {
    return (
      <input
        value={values.filename}
        onChange={(event) => update({ filename: event.target.value })}
        className={toolWideInputClass}
        placeholder="输出文件（可选）"
        aria-label="快照输出文件"
      />
    );
  }
  if (action === 'click_target') {
    return (
      <>
        <input
          value={values.target}
          onChange={(event) => update({ target: event.target.value })}
          className={toolInputClass}
          placeholder="目标"
          aria-label="点击目标"
        />
        <input
          value={values.element}
          onChange={(event) => update({ element: event.target.value })}
          className={toolInputClass}
          placeholder="元素（可选）"
          aria-label="元素描述"
        />
        <select
          value={values.button}
          onChange={(event) => update({ button: event.target.value })}
          className={toolSelectClass}
          aria-label="鼠标按键"
        >
          <option value="">默认按键</option>
          <option value="left">左键</option>
          <option value="right">右键</option>
          <option value="middle">中键</option>
        </select>
        <EffectSelect value={values.effect} onChange={(effect) => update({ effect })} />
        <ToolCheckbox
          label="双击"
          checked={values.doubleClick}
          onChange={(doubleClick) => update({ doubleClick })}
        />
      </>
    );
  }
  if (action === 'fill') {
    return (
      <>
        <input
          value={values.target}
          onChange={(event) => update({ target: event.target.value })}
          className={toolInputClass}
          placeholder="目标"
          aria-label="填写目标"
        />
        <input
          value={values.text}
          onChange={(event) => update({ text: event.target.value })}
          className={toolWideInputClass}
          placeholder="文本"
          aria-label="填写文本"
        />
        <input
          value={values.element}
          onChange={(event) => update({ element: event.target.value })}
          className={toolInputClass}
          placeholder="元素（可选）"
          aria-label="字段描述"
        />
        <EffectSelect value={values.effect} onChange={(effect) => update({ effect })} />
        <ToolCheckbox
          label="提交"
          checked={values.submit}
          onChange={(submit) => update({ submit })}
        />
        <ToolCheckbox
          label="逐字"
          checked={values.slowly}
          onChange={(slowly) => update({ slowly })}
        />
      </>
    );
  }
  if (action === 'type_at') {
    return (
      <>
        <CoordinateInput label="X 坐标" value={values.x} onChange={(x) => update({ x })} />
        <CoordinateInput label="Y 坐标" value={values.y} onChange={(y) => update({ y })} />
        <input
          value={values.text}
          onChange={(event) => update({ text: event.target.value })}
          className={toolWideInputClass}
          placeholder="文本"
          aria-label="输入文本"
        />
        <EffectSelect value={values.effect} onChange={(effect) => update({ effect })} />
        <ToolCheckbox
          label="提交"
          checked={values.submit}
          onChange={(submit) => update({ submit })}
        />
        <ToolCheckbox
          label="逐字"
          checked={values.slowly}
          onChange={(slowly) => update({ slowly })}
        />
      </>
    );
  }
  if (action === 'console') {
    return (
      <>
        <select
          value={values.level}
          onChange={(event) => update({ level: event.target.value })}
          className={toolSelectClass}
          aria-label="控制台级别"
        >
          <option value="">全部级别</option>
          <option value="error">错误</option>
          <option value="warning">警告</option>
          <option value="info">信息</option>
          <option value="debug">调试</option>
        </select>
        <input
          value={values.contains}
          onChange={(event) => update({ contains: event.target.value })}
          className={toolWideInputClass}
          placeholder="包含文本（可选）"
          aria-label="控制台文本过滤"
        />
      </>
    );
  }
  if (action === 'network') {
    return (
      <>
        <input
          value={values.method}
          onChange={(event) => update({ method: event.target.value.toUpperCase() })}
          className={toolShortInputClass}
          placeholder="方法"
          aria-label="请求方法"
        />
        <input
          type="number"
          min={100}
          max={599}
          value={values.status}
          onChange={(event) => update({ status: event.target.value })}
          className={toolShortInputClass}
          placeholder="状态"
          aria-label="响应状态"
        />
        <input
          value={values.contains}
          onChange={(event) => update({ contains: event.target.value })}
          className={toolWideInputClass}
          placeholder="包含文本（可选）"
          aria-label="网络文本过滤"
        />
      </>
    );
  }
  if (action === 'dom_inspect') {
    return (
      <>
        <input
          value={values.target}
          onChange={(event) => update({ target: event.target.value })}
          className={toolInputClass}
          placeholder="目标（可选）"
          aria-label="DOM 目标"
        />
        <input
          value={values.text}
          onChange={(event) => update({ text: event.target.value })}
          className={toolWideInputClass}
          placeholder="文本（可选）"
          aria-label="DOM 文本"
        />
        <input
          type="number"
          min={1}
          max={12}
          value={values.maxDepth}
          onChange={(event) => update({ maxDepth: event.target.value })}
          className={toolShortInputClass}
          placeholder="深度"
          aria-label="最大深度"
        />
      </>
    );
  }
  return (
    <>
      <select
        value={values.traceAction}
        onChange={(event) => update({ traceAction: event.target.value })}
        className={toolSelectClass}
        aria-label="跟踪操作"
      >
        <option value="start">开始</option>
        <option value="stop">停止</option>
      </select>
      <input
        value={values.path}
        onChange={(event) => update({ path: event.target.value })}
        className={toolWideInputClass}
        placeholder="输出路径（可选）"
        aria-label="跟踪输出路径"
      />
    </>
  );
}

function EffectSelect({ value, onChange }: { value: string; onChange: (value: string) => void }) {
  return (
    <select
      value={value}
      onChange={(event) => onChange(event.target.value)}
      className={toolSelectClass}
      aria-label="外部影响"
    >
      <option value="none">无外部影响</option>
      <option value="sensitive_submit">敏感提交</option>
      <option value="purchase">购买</option>
      <option value="publish">发布</option>
      <option value="send_message">发送消息</option>
      <option value="account_change">账户变更</option>
      <option value="permission_change">权限变更</option>
      <option value="cloud_delete">云端删除</option>
    </select>
  );
}

function ToolCheckbox({
  label,
  checked,
  onChange,
}: {
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className={toolToggleClass}>
      <input
        type="checkbox"
        checked={checked}
        onChange={(event) => onChange(event.target.checked)}
      />
      {label}
    </label>
  );
}

function CoordinateInput({
  label,
  value,
  onChange,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <input
      type="number"
      min={0}
      value={value}
      onChange={(event) => onChange(event.target.value)}
      className={toolShortInputClass}
      placeholder={label}
      aria-label={label}
    />
  );
}

export function browserToolCommand(
  action: BrowserToolAction,
  values: BrowserToolValues
): BrowserCommand {
  if (action === 'status') return { action };
  if (action === 'snapshot') {
    return { action, filename: optionalText(values.filename) };
  }
  if (action === 'click_target') {
    return {
      action,
      target: values.target.trim(),
      element: optionalText(values.element),
      button: optionalText(values.button),
      double_click: values.doubleClick,
      effect: values.effect,
    };
  }
  if (action === 'fill') {
    return {
      action,
      target: values.target.trim(),
      text: values.text,
      element: optionalText(values.element),
      submit: values.submit,
      slowly: values.slowly,
      effect: values.effect,
    };
  }
  if (action === 'type_at') {
    return {
      action,
      x: finiteNumber(values.x),
      y: finiteNumber(values.y),
      text: values.text,
      submit: values.submit,
      slowly: values.slowly,
      effect: values.effect,
    };
  }
  if (action === 'console') {
    return {
      action,
      level: optionalText(values.level),
      contains: optionalText(values.contains),
    };
  }
  if (action === 'network') {
    return {
      action,
      method: optionalText(values.method),
      status: optionalInteger(values.status),
      contains: optionalText(values.contains),
    };
  }
  if (action === 'dom_inspect') {
    return {
      action,
      target: optionalText(values.target),
      text: optionalText(values.text),
      max_depth: optionalInteger(values.maxDepth),
    };
  }
  return {
    action,
    trace_action: values.traceAction,
    path: optionalText(values.path),
  };
}

function browserToolCommandReady(action: BrowserToolAction, values: BrowserToolValues) {
  if (action === 'click_target') return Boolean(values.target.trim());
  if (action === 'fill') return Boolean(values.target.trim() && values.text);
  if (action === 'type_at') {
    return (
      Number.isFinite(Number(values.x)) && Number.isFinite(Number(values.y)) && Boolean(values.text)
    );
  }
  return true;
}

function optionalText(value: string) {
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

function optionalInteger(value: string) {
  if (!value.trim()) return null;
  const number = Number(value);
  return Number.isInteger(number) && number >= 0 ? number : null;
}

function finiteNumber(value: string) {
  const number = Number(value);
  return Number.isFinite(number) ? number : 0;
}

const toolInputClass =
  'h-7 w-28 min-w-0 rounded-md border border-[var(--border-primary)] bg-[var(--bg-primary)] px-2 text-[11px] text-[var(--text-primary)] outline-none focus:border-[var(--accent)]';
const toolWideInputClass = `${toolInputClass} flex-1 basis-36`;
const toolShortInputClass = `${toolInputClass} w-20`;
const toolSelectClass =
  'h-7 shrink-0 rounded-md border border-[var(--border-primary)] bg-[var(--bg-primary)] px-1.5 text-[11px] text-[var(--text-secondary)] outline-none focus:border-[var(--accent)]';
const toolToggleClass =
  'inline-flex h-7 shrink-0 items-center gap-1.5 px-1.5 text-[11px] text-[var(--text-secondary)]';
const toolIconButtonClass =
  'flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:opacity-35';
