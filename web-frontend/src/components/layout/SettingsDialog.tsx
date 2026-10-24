import { lazy, Suspense, useMemo, useState } from 'react';
import type { CSSProperties } from 'react';
import {
  X,
  Settings,
  Save,
  Archive,
  ShieldCheck,
  Minimize2,
  Wrench,
  Globe,
  BookOpen,
  Brain,
  Cpu,
  Database,
  Activity,
  BrainCircuit,
  Sparkles,
  Package,
  Webhook,
  Timer,
  GitBranch,
  LayoutDashboard,
  ChevronDown,
  ChevronRight,
} from 'lucide-react';
import { useUiStore, type SettingsTabId } from '../../stores/uiStore';
import { Modal } from '../common/Modal';
import { ErrorBoundary } from '../common/ErrorBoundary';

// Keep panel identities at module scope; closed settings should not eagerly load every screen.
const ConfigPanel = lazy(() =>
  import('../config/ConfigPanel').then((module) => ({ default: module.ConfigPanel }))
);
const SessionsPanel = lazy(() =>
  import('../sessions/SessionsPanel').then((module) => ({ default: module.SessionsPanel }))
);
const ArchivedConversationsPanel = lazy(() =>
  import('../sessions/ArchivedConversationsPanel').then((module) => ({
    default: module.ArchivedConversationsPanel,
  }))
);
const AuditPanel = lazy(() =>
  import('../audit/AuditPanel').then((module) => ({ default: module.AuditPanel }))
);
const CompressPanel = lazy(() =>
  import('../compress/CompressPanel').then((module) => ({ default: module.CompressPanel }))
);
const ToolsPanel = lazy(() =>
  import('../tools/ToolsPanel').then((module) => ({ default: module.ToolsPanel }))
);
const McpPanel = lazy(() =>
  import('../mcp/McpPanel').then((module) => ({ default: module.McpPanel }))
);
const SkillsPanel = lazy(() =>
  import('../skills/SkillsPanel').then((module) => ({ default: module.SkillsPanel }))
);
const MemoryPanel = lazy(() =>
  import('../memory/MemoryPanel').then((module) => ({ default: module.MemoryPanel }))
);
const EvolutionPanel = lazy(() =>
  import('../evolution/EvolutionPanel').then((module) => ({ default: module.EvolutionPanel }))
);
const ProviderPanel = lazy(() =>
  import('../providers/ProviderPanel').then((module) => ({ default: module.ProviderPanel }))
);
const PluginPanel = lazy(() =>
  import('../plugins/PluginPanel').then((module) => ({ default: module.PluginPanel }))
);
const HooksPanel = lazy(() =>
  import('../hooks/HooksPanel').then((module) => ({ default: module.HooksPanel }))
);
const SchedulerPanel = lazy(() =>
  import('../scheduler/SchedulerPanel').then((module) => ({ default: module.SchedulerPanel }))
);
const WorktreePanel = lazy(() =>
  import('../coding/WorktreePanel').then((module) => ({ default: module.WorktreePanel }))
);
const ObservabilityPanel = lazy(() =>
  import('../observability/ObservabilityPanel').then((module) => ({
    default: module.ObservabilityPanel,
  }))
);
const SandboxPanel = lazy(() =>
  import('../sandbox/SandboxPanel').then((module) => ({ default: module.SandboxPanel }))
);
interface SettingsItem {
  id: SettingsTabId;
  label: string;
  icon: typeof Settings;
  maturity: 'core' | 'live' | 'advanced' | 'lab';
  description: string;
}

interface SettingsGroup {
  label: string;
  icon: typeof Settings;
  tier: 'core' | 'advanced';
  defaultOpen?: boolean;
  items: SettingsItem[];
}

const overviewItem: SettingsItem = {
  id: 'overview',
  label: '总览',
  icon: LayoutDashboard,
  maturity: 'core',
  description: '核心工作流入口',
};

const settingsGroups: SettingsGroup[] = [
  {
    label: '核心工作流',
    icon: Cpu,
    tier: 'core',
    defaultOpen: true,
    items: [
      {
        id: 'providers',
        label: '模型',
        icon: Cpu,
        maturity: 'core',
        description: '模型、供应商和默认模型',
      },
      {
        id: 'tools',
        label: '工具',
        icon: Wrench,
        maturity: 'core',
        description: 'Agent 可用工具与权限',
      },
      { id: 'mcp', label: 'MCP', icon: Globe, maturity: 'core', description: '本地扩展服务连接' },
      {
        id: 'skills',
        label: '技能',
        icon: BookOpen,
        maturity: 'core',
        description: '可加载的能力包',
      },
      {
        id: 'plugins',
        label: '插件',
        icon: Package,
        maturity: 'core',
        description: '本地插件市场',
      },
      {
        id: 'hooks',
        label: 'Hooks',
        icon: Webhook,
        maturity: 'core',
        description: '事件钩子来源与规则',
      },
      {
        id: 'observability',
        label: '运行观测',
        icon: Activity,
        maturity: 'core',
        description: 'Token、缓存、trace 与诊断',
      },
      { id: 'memory', label: '记忆', icon: Brain, maturity: 'live', description: '项目与用户记忆' },
    ],
  },
  {
    label: '项目数据',
    icon: Database,
    tier: 'core',
    defaultOpen: true,
    items: [
      {
        id: 'sessions',
        label: '会话',
        icon: Save,
        maturity: 'live',
        description: '会话历史与恢复',
      },
      {
        id: 'archives',
        label: '归档',
        icon: Archive,
        maturity: 'live',
        description: '恢复或永久删除归档会话',
      },
    ],
  },
  {
    label: '治理',
    icon: ShieldCheck,
    tier: 'core',
    defaultOpen: true,
    items: [
      {
        id: 'audit',
        label: '审计',
        icon: ShieldCheck,
        maturity: 'live',
        description: '审批、工具与风险日志',
      },
      {
        id: 'sandbox',
        label: '沙箱',
        icon: ShieldCheck,
        maturity: 'live',
        description: '本地代码执行与隔离验证',
      },
    ],
  },
  {
    label: '高级与实验',
    icon: Sparkles,
    tier: 'advanced',
    items: [
      {
        id: 'compress',
        label: '压缩',
        icon: Minimize2,
        maturity: 'advanced',
        description: '上下文压缩与摘要',
      },
      {
        id: 'config',
        label: '配置',
        icon: Settings,
        maturity: 'advanced',
        description: '底层应用配置',
      },
      {
        id: 'worktree',
        label: 'Worktree',
        icon: GitBranch,
        maturity: 'advanced',
        description: '并行开发工作区',
      },
      {
        id: 'scheduler',
        label: '定时任务',
        icon: Timer,
        maturity: 'advanced',
        description: '后台计划任务',
      },
      {
        id: 'evolution',
        label: '自进化',
        icon: Sparkles,
        maturity: 'lab',
        description: '实验性自我改进流程',
      },
    ],
  },
];

const panels: Record<SettingsTabId, React.FC> = {
  overview: SettingsOverview,
  tools: ToolsPanel,
  mcp: McpPanel,
  skills: SkillsPanel,
  memory: MemoryPanel,

  config: ConfigPanel,
  providers: ProviderPanel,
  sessions: SessionsPanel,
  archives: ArchivedConversationsPanel,
  audit: AuditPanel,
  sandbox: SandboxPanel,
  observability: ObservabilityPanel,
  compress: CompressPanel,
  evolution: EvolutionPanel,
  plugins: PluginPanel,
  hooks: HooksPanel,
  scheduler: SchedulerPanel,
  worktree: WorktreePanel,
};

function SettingsOverview() {
  const setActiveSettingsTab = useUiStore((s) => s.setActiveSettingsTab);
  const workflows: Array<{
    title: string;
    icon: typeof Settings;
    items: SettingsItem[];
  }> = [
    {
      title: '运行与记录',
      icon: BrainCircuit,
      items: [
        settingsGroups[0].items.find((item) => item.id === 'observability') ?? overviewItem,
        settingsGroups[2].items.find((item) => item.id === 'audit') ?? overviewItem,
        settingsGroups[2].items.find((item) => item.id === 'sandbox') ?? overviewItem,
      ],
    },
    {
      title: '模型与工具',
      icon: Cpu,
      items: [
        settingsGroups[0].items.find((item) => item.id === 'providers') ?? overviewItem,
        settingsGroups[0].items.find((item) => item.id === 'tools') ?? overviewItem,
        settingsGroups[0].items.find((item) => item.id === 'mcp') ?? overviewItem,
        settingsGroups[0].items.find((item) => item.id === 'skills') ?? overviewItem,
        settingsGroups[0].items.find((item) => item.id === 'plugins') ?? overviewItem,
        settingsGroups[0].items.find((item) => item.id === 'hooks') ?? overviewItem,
      ],
    },
    {
      title: '上下文与历史',
      icon: Database,
      items: [
        settingsGroups[0].items.find((item) => item.id === 'memory') ?? overviewItem,
        settingsGroups[1].items.find((item) => item.id === 'sessions') ?? overviewItem,
        settingsGroups[3].items.find((item) => item.id === 'compress') ?? overviewItem,
      ],
    },
    {
      title: '高级扩展',
      icon: Sparkles,
      items: [
        settingsGroups[3].items.find((item) => item.id === 'scheduler') ?? overviewItem,
        settingsGroups[3].items.find((item) => item.id === 'evolution') ?? overviewItem,
      ],
    },
  ];

  return (
    <div className="grid items-start gap-4 md:grid-cols-2">
      {workflows.map((workflow) => (
        <section
          key={workflow.title}
          className="rounded-xl border border-[var(--border-primary)] bg-[var(--bg-secondary)] p-4"
        >
          <div className="mb-3 flex items-center gap-2">
            <workflow.icon size={16} className="text-[var(--text-secondary)]" />
            <h3 className="text-sm font-semibold text-[var(--text-primary)]">{workflow.title}</h3>
          </div>
          <div className="space-y-1">
            {workflow.items.map(({ id, label, icon: Icon, maturity, description }) => (
              <button
                key={`${workflow.title}-${id}`}
                type="button"
                onClick={() => setActiveSettingsTab(id)}
                className="flex w-full items-start gap-3 rounded-lg px-3 py-2.5 text-left transition-colors hover:bg-[var(--bg-hover)]"
              >
                <Icon size={15} className="mt-0.5 shrink-0 text-[var(--text-tertiary)]" />
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-2">
                    <span className="truncate text-sm font-medium text-[var(--text-primary)]">
                      {label}
                    </span>
                    {maturity === 'lab' && (
                      <span className="shrink-0 rounded-md bg-[var(--bg-hover)] px-2 py-0.5 text-[11px] text-[var(--text-secondary)]">
                        实验
                      </span>
                    )}
                  </span>
                  <span className="mt-1 block text-xs leading-relaxed text-[var(--text-secondary)]">
                    {description}
                  </span>
                </span>
              </button>
            ))}
          </div>
        </section>
      ))}
    </div>
  );
}

export function SettingsDialog() {
  const { settingsOpen, closeSettings, activeSettingsTab, setActiveSettingsTab } = useUiStore();
  const allItems = useMemo(
    () => [overviewItem, ...settingsGroups.flatMap((group) => group.items)],
    []
  );
  const activeItem = allItems.find((i) => i.id === activeSettingsTab);
  const effectiveSettingsTab = activeItem ? activeSettingsTab : 'overview';
  const effectiveItem = activeItem ?? overviewItem;
  const Panel = panels[effectiveSettingsTab];
  const [expandedAdvanced, setExpandedAdvanced] = useState<Record<string, boolean>>({});
  const activeGroup = settingsGroups.find((group) =>
    group.items.some((item) => item.id === effectiveSettingsTab)
  );

  if (!settingsOpen) return null;

  return (
    <Modal
      onClose={closeSettings}
      ariaLabelledBy="settings-dialog-title"
      className="settings-dialog animate-scale-in flex h-[85vh] w-[92vw] max-w-6xl overflow-hidden rounded-xl border border-[var(--border-primary)] bg-[var(--settings-dialog-bg)] shadow-[var(--shadow-xl)] max-sm:h-screen max-sm:w-screen max-sm:max-w-none max-sm:rounded-none"
      style={
        {
          '--accent': 'var(--settings-accent)',
          '--accent-bg': 'var(--settings-accent-bg)',
          '--border-focus': 'var(--settings-accent)',
          '--text-on-accent': 'var(--settings-text-on-accent)',
        } as CSSProperties
      }
    >
      {/* Left sidebar — settings nav */}
      <div className="flex w-[220px] shrink-0 flex-col border-r border-[var(--border-primary)] bg-[var(--settings-sidebar-bg)]">
        <div className="flex items-center justify-between border-b border-[var(--border-secondary)] px-5 py-4">
          <h2
            id="settings-dialog-title"
            className="text-sm font-semibold text-[var(--text-primary)]"
          >
            设置
          </h2>
          <button
            type="button"
            onClick={closeSettings}
            aria-label="关闭设置"
            className="flex h-8 w-8 items-center justify-center rounded-lg text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
          >
            <X size={15} />
          </button>
        </div>
        <nav className="flex-1 overflow-y-auto p-3 space-y-5">
          <button
            type="button"
            aria-current={effectiveSettingsTab === 'overview' ? 'page' : undefined}
            onClick={() => setActiveSettingsTab('overview')}
            className={`flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left transition-all duration-150 ${
              effectiveSettingsTab === 'overview'
                ? 'bg-[var(--settings-active-bg)] text-[var(--text-primary)]'
                : 'text-[var(--text-secondary)] hover:bg-[var(--bg-sidebar-hover)] hover:text-[var(--text-primary)]'
            }`}
          >
            <LayoutDashboard size={15} className="shrink-0" />
            <span className="min-w-0 flex-1">
              <span className="truncate text-sm font-medium">总览</span>
            </span>
          </button>
          {settingsGroups.map((group) => (
            <div key={group.label}>
              {/* Group header */}
              <button
                type="button"
                onClick={() => {
                  if (group.tier === 'advanced') {
                    setExpandedAdvanced((prev) => ({
                      ...prev,
                      [group.label]: !prev[group.label],
                    }));
                  }
                }}
                className="flex w-full items-center gap-2 px-2.5 pb-2 text-left"
              >
                <group.icon size={12} className="text-[var(--text-tertiary)]" />
                <span className="flex-1 text-xs font-semibold text-[var(--text-secondary)]">
                  {group.label}
                </span>
                {group.tier === 'advanced' &&
                  (expandedAdvanced[group.label] || activeGroup?.label === group.label ? (
                    <ChevronDown size={12} className="text-[var(--text-tertiary)]" />
                  ) : (
                    <ChevronRight size={12} className="text-[var(--text-tertiary)]" />
                  ))}
              </button>
              {(group.tier === 'core' ||
                group.defaultOpen ||
                expandedAdvanced[group.label] ||
                activeGroup?.label === group.label) &&
                group.items.map(({ id, label, icon: Icon, maturity, description }) => (
                  <button
                    key={id}
                    type="button"
                    title={description}
                    aria-current={effectiveSettingsTab === id ? 'page' : undefined}
                    onClick={() => setActiveSettingsTab(id)}
                    className={`flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left transition-all duration-150
                    ${
                      effectiveSettingsTab === id
                        ? 'bg-[var(--settings-active-bg)] text-[var(--text-primary)]'
                        : 'text-[var(--text-secondary)] hover:bg-[var(--bg-sidebar-hover)] hover:text-[var(--text-primary)]'
                    }`}
                  >
                    <Icon size={16} className="shrink-0" />
                    <span className="min-w-0 flex-1">
                      <span className="flex min-w-0 items-center gap-2">
                        <span className="truncate text-sm font-medium">{label}</span>
                        {maturity === 'lab' && (
                          <span className="shrink-0 rounded-md bg-[var(--bg-hover)] px-2 py-0.5 text-[11px] text-[var(--text-secondary)]">
                            实验
                          </span>
                        )}
                      </span>
                    </span>
                  </button>
                ))}
            </div>
          ))}
        </nav>
      </div>

      {/* Content area */}
      <div className="flex flex-1 flex-col min-w-0 bg-[var(--settings-content-bg)]">
        <div className="flex items-center justify-between border-b border-[var(--border-secondary)] bg-[var(--settings-panel-bg)] px-6 py-3.5">
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <span className="text-sm font-semibold text-[var(--text-primary)]">
                {effectiveItem?.label}
              </span>
              {effectiveItem.maturity === 'lab' && (
                <span className="rounded-md bg-[var(--bg-hover)] px-2 py-0.5 text-[11px] text-[var(--text-secondary)]">
                  实验
                </span>
              )}
            </div>
            {effectiveItem && (
              <div className="mt-1 text-xs text-[var(--text-secondary)]">
                {effectiveItem.description}
              </div>
            )}
          </div>
        </div>
        <div className="flex-1 overflow-y-auto">
          <div className="animate-fade-up p-6">
            {/* A failed optional screen must leave the workspace and settings navigation mounted. */}
            <ErrorBoundary
              key={effectiveSettingsTab}
              fallback={
                <div role="alert" className="space-y-3 text-sm text-[var(--text-secondary)]">
                  <p>此设置页面加载失败。你可以返回总览、关闭设置，或刷新页面后再试。</p>
                  <div className="flex gap-2">
                    <button
                      type="button"
                      className="btn btn-secondary"
                      onClick={() => setActiveSettingsTab('overview')}
                    >
                      返回总览
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary"
                      onClick={() => window.location.reload()}
                    >
                      刷新页面
                    </button>
                  </div>
                </div>
              }
            >
              <Suspense
                fallback={
                  <div
                    role="status"
                    className="flex min-h-40 items-center justify-center text-sm text-[var(--text-secondary)]"
                  >
                    正在加载…
                  </div>
                }
              >
                <Panel />
              </Suspense>
            </ErrorBoundary>
          </div>
        </div>
      </div>
    </Modal>
  );
}
