import { useEffect, useMemo, useState } from 'react';
import type { CSSProperties } from 'react';
import {
  X,
  Settings,
  Save,
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
  Timer,
  FileEdit,
  GitBranch,
  Scale,
  LayoutDashboard,
  ChevronDown,
  ChevronRight,
} from 'lucide-react';
import { useUiStore, type SettingsTabId } from '../../stores/uiStore';
import { ConfigPanel } from '../config/ConfigPanel';
import { SessionsPanel } from '../sessions/SessionsPanel';
import { AuditPanel } from '../audit/AuditPanel';
import { CompressPanel } from '../compress/CompressPanel';
import { ToolsPanel } from '../tools/ToolsPanel';
import { McpPanel } from '../mcp/McpPanel';
import { SkillsPanel } from '../skills/SkillsPanel';
import { MemoryPanel } from '../memory/MemoryPanel';
import { EvolutionPanel } from '../evolution/EvolutionPanel';
import { ProviderPanel } from '../providers/ProviderPanel';
import { PluginPanel } from '../plugins/PluginPanel';
import { SchedulerPanel } from '../scheduler/SchedulerPanel';
import { ScratchpadPanel } from '../scratchpad/ScratchpadPanel';
import { DecisionLogPanel } from '../decisions/DecisionLogPanel';
import { WorktreePanel } from '../coding/WorktreePanel';
import { ObservabilityPanel } from '../observability/ObservabilityPanel';
import { SandboxPanel } from '../sandbox/SandboxPanel';

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
      { id: 'providers', label: '模型', icon: Cpu, maturity: 'core', description: '模型、供应商和默认模型' },
      { id: 'tools', label: '工具', icon: Wrench, maturity: 'core', description: 'Agent 可用工具与权限' },
      { id: 'mcp', label: 'MCP', icon: Globe, maturity: 'core', description: '本地扩展服务连接' },
      { id: 'observability', label: '运行观测', icon: Activity, maturity: 'core', description: 'Token、缓存、trace 与诊断' },
      { id: 'memory', label: '记忆', icon: Brain, maturity: 'live', description: '项目与用户记忆' },
    ],
  },
  {
    label: '项目数据',
    icon: Database,
    tier: 'core',
    defaultOpen: true,
    items: [
      { id: 'sessions', label: '会话', icon: Save, maturity: 'live', description: '会话历史与恢复' },
      { id: 'decisions', label: '决策', icon: Scale, maturity: 'live', description: '关键决策记录' },
    ],
  },
  {
    label: '治理',
    icon: ShieldCheck,
    tier: 'core',
    defaultOpen: true,
    items: [
      { id: 'audit', label: '审计', icon: ShieldCheck, maturity: 'live', description: '审批、工具与风险日志' },
      { id: 'sandbox', label: '沙箱', icon: ShieldCheck, maturity: 'live', description: '本地代码执行与隔离验证' },
    ],
  },
  {
    label: '高级与实验',
    icon: Sparkles,
    tier: 'advanced',
    items: [
      { id: 'scratchpad', label: '草稿', icon: FileEdit, maturity: 'advanced', description: '临时笔记与中间材料' },
      { id: 'compress', label: '压缩', icon: Minimize2, maturity: 'advanced', description: '上下文压缩与摘要' },
      { id: 'config', label: '配置', icon: Settings, maturity: 'advanced', description: '底层应用配置' },
      { id: 'worktree', label: 'Worktree', icon: GitBranch, maturity: 'advanced', description: '并行开发工作区' },
      { id: 'skills', label: '技能', icon: BookOpen, maturity: 'advanced', description: '可加载的能力包' },
      { id: 'plugins', label: '插件', icon: Package, maturity: 'advanced', description: '本地插件市场' },
      { id: 'scheduler', label: '定时任务', icon: Timer, maturity: 'advanced', description: '后台计划任务' },
      { id: 'evolution', label: '自进化', icon: Sparkles, maturity: 'lab', description: '实验性自我改进流程' },
    ],
  },
];

const maturityLabel: Record<SettingsItem['maturity'], string> = {
  core: '核心',
  live: '可用',
  advanced: '高级',
  lab: '实验',
};

const panels: Record<SettingsTabId, React.FC> = {
  overview: SettingsOverview,
  tools: ToolsPanel,
  mcp: McpPanel,
  skills: SkillsPanel,
  memory: MemoryPanel,

  config: ConfigPanel,
  providers: ProviderPanel,
  sessions: SessionsPanel,
  audit: AuditPanel,
  sandbox: SandboxPanel,
  scratchpad: ScratchpadPanel,
  decisions: DecisionLogPanel,
  observability: ObservabilityPanel,
  compress: CompressPanel,
  evolution: EvolutionPanel,
  plugins: PluginPanel,
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
      title: '执行主链路',
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
        settingsGroups[3].items.find((item) => item.id === 'skills') ?? overviewItem,
        settingsGroups[3].items.find((item) => item.id === 'plugins') ?? overviewItem,
        settingsGroups[3].items.find((item) => item.id === 'scheduler') ?? overviewItem,
      ],
    },
  ];

  return (
    <div className="grid gap-4 md:grid-cols-2">
      {workflows.map((workflow) => (
        <section
          key={workflow.title}
          className="rounded-lg border border-[var(--border-primary)] bg-[var(--bg-secondary)] p-4"
        >
          <div className="mb-3 flex items-center gap-2">
            <workflow.icon size={16} className="text-[var(--text-secondary)]" />
            <h3 className="text-sm font-semibold text-[var(--text-primary)]">
              {workflow.title}
            </h3>
          </div>
          <div className="space-y-2">
            {workflow.items.map(({ id, label, icon: Icon, maturity, description }) => (
              <button
                key={`${workflow.title}-${id}`}
                type="button"
                onClick={() => setActiveSettingsTab(id)}
                className="flex w-full items-start gap-3 rounded-md px-3 py-2 text-left transition-colors hover:bg-[var(--bg-hover)]"
              >
                <Icon size={15} className="mt-0.5 shrink-0 text-[var(--text-tertiary)]" />
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-2">
                    <span className="truncate text-sm font-medium text-[var(--text-primary)]">
                      {label}
                    </span>
                    <span className="shrink-0 rounded-md bg-[var(--bg-hover)] px-1.5 py-0.5 text-[9px] text-[var(--text-tertiary)]">
                      {maturityLabel[maturity]}
                    </span>
                  </span>
                  <span className="mt-0.5 block truncate text-xs text-[var(--text-tertiary)]">
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

  // Close on Escape key
  useEffect(() => {
    if (!settingsOpen) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closeSettings();
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [settingsOpen, closeSettings]);

  if (!settingsOpen) return null;

  return (
    <>
      {/* Backdrop */}
      <div
        className="fixed inset-0 z-50"
        style={{ background: 'var(--bg-overlay)' }}
        onClick={closeSettings}
      />

      {/* Dialog */}
      <div
        className="settings-dialog animate-scale-in fixed left-1/2 top-1/2 z-50 flex h-[85vh] w-[92vw] max-w-6xl -translate-x-1/2 -translate-y-1/2 overflow-hidden rounded-xl border border-[var(--border-primary)] bg-[var(--settings-dialog-bg)] shadow-[var(--shadow-xl)] max-sm:h-screen max-sm:w-screen max-sm:max-w-none max-sm:rounded-none"
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
            <h2 className="text-sm font-semibold tracking-tight text-[var(--text-primary)]">
              设置
            </h2>
            <button
              onClick={closeSettings}
              className="flex h-7 w-7 items-center justify-center rounded-lg text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            >
              <X size={15} />
            </button>
          </div>
          <nav className="flex-1 overflow-y-auto p-3 space-y-5">
            <button
              onClick={() => setActiveSettingsTab('overview')}
              className={`flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left transition-all duration-150 ${
                effectiveSettingsTab === 'overview'
                  ? 'bg-[var(--settings-active-bg)] text-[var(--text-primary)]'
                  : 'text-[var(--text-secondary)] hover:bg-[var(--bg-sidebar-hover)] hover:text-[var(--text-primary)]'
              }`}
            >
              <LayoutDashboard size={15} className="shrink-0" />
              <span className="min-w-0 flex-1">
                <span className="truncate text-[13px] font-medium">总览</span>
                <span className="mt-0.5 block truncate text-[10px] font-normal text-[var(--text-tertiary)]">
                  核心工作流入口
                </span>
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
                  <span className="flex-1 text-[11px] font-semibold uppercase tracking-widest text-[var(--text-tertiary)]">
                    {group.label}
                  </span>
                  {group.tier === 'advanced' &&
                    (expandedAdvanced[group.label] ||
                    activeGroup?.label === group.label ? (
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
                    onClick={() => setActiveSettingsTab(id)}
                    className={`flex w-full items-start gap-3 rounded-lg px-3 py-2.5 text-left transition-all duration-150
                      ${
                        effectiveSettingsTab === id
                          ? 'bg-[var(--settings-active-bg)] text-[var(--text-primary)]'
                          : 'text-[var(--text-secondary)] hover:bg-[var(--bg-sidebar-hover)] hover:text-[var(--text-primary)]'
                      }`}
                  >
                    <Icon size={15} className="mt-0.5 shrink-0" />
                    <span className="min-w-0 flex-1">
                      <span className="flex min-w-0 items-center gap-2">
                        <span className="truncate text-[13px] font-medium">{label}</span>
                        <span className="shrink-0 rounded-md bg-[var(--bg-hover)] px-1.5 py-0.5 text-[9px] text-[var(--text-tertiary)]">
                          {maturityLabel[maturity]}
                        </span>
                      </span>
                      <span className="mt-0.5 block truncate text-[10px] font-normal text-[var(--text-tertiary)]">
                        {description}
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
                {effectiveItem && (
                  <span className="rounded-md bg-[var(--bg-hover)] px-1.5 py-0.5 text-[10px] text-[var(--text-tertiary)]">
                    {maturityLabel[effectiveItem.maturity]}
                  </span>
                )}
              </div>
              {effectiveItem && (
                <div className="mt-0.5 text-xs text-[var(--text-tertiary)]">
                  {effectiveItem.description}
                </div>
              )}
            </div>
          </div>
          <div className="flex-1 overflow-y-auto">
            <div className="animate-fade-up p-6">
              <Panel />
            </div>
          </div>
        </div>
      </div>
    </>
  );
}
