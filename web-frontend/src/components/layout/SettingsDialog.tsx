import { useEffect } from 'react';
import { X, Settings, Save, Lock, ShieldCheck, GitBranch, Terminal, Minimize2, FileJson, Wrench, Globe, BookOpen, Brain, Cpu, Database, Activity, Sparkles, Package, Timer, BrainCircuit, GitFork } from 'lucide-react';
import { useUiStore, type SettingsTabId } from '../../stores/uiStore';
import { ConfigPanel } from '../config/ConfigPanel';
import { SessionsPanel } from '../sessions/SessionsPanel';
import { PermissionsPanel } from '../permissions/PermissionsPanel';
import { AuditPanel } from '../audit/AuditPanel';
import { WorkflowPanel } from '../workflow/WorkflowPanel';
import { SandboxPanel } from '../sandbox/SandboxPanel';
import { CompressPanel } from '../compress/CompressPanel';
import { ExtractPanel } from '../extract/ExtractPanel';
import { ToolsPanel } from '../tools/ToolsPanel';
import { McpPanel } from '../mcp/McpPanel';
import { SkillsPanel } from '../skills/SkillsPanel';
import { MemoryPanel } from '../memory/MemoryPanel';
import { EvolutionPanel } from '../evolution/EvolutionPanel';
import { ProviderPanel } from '../providers/ProviderPanel';
import { PluginPanel } from '../plugins/PluginPanel';
import { SchedulerPanel } from '../scheduler/SchedulerPanel';
import { AutoMemoryPanel } from '../memory/AutoMemoryPanel';
import { WorktreePanel } from '../coding/WorktreePanel';

interface SettingsItem {
  id: SettingsTabId;
  label: string;
  icon: typeof Settings;
}

const settingsGroups: { label: string; icon: typeof Settings; items: SettingsItem[] }[] = [
  {
    label: '智能体',
    icon: Cpu,
    items: [
      { id: 'config', label: '配置', icon: Settings },
      { id: 'providers', label: '模型供应商', icon: Cpu },
      { id: 'tools', label: '工具', icon: Wrench },
      { id: 'mcp', label: 'MCP', icon: Globe },
      { id: 'skills', label: '技能', icon: BookOpen },
      { id: 'plugins', label: '插件', icon: Package },
      { id: 'memory', label: '记忆', icon: Brain },
      { id: 'auto-memory', label: '自动记忆', icon: BrainCircuit },
    ],
  },
  {
    label: '数据',
    icon: Database,
    items: [
      { id: 'sessions', label: '会话', icon: Save },
      { id: 'compress', label: '压缩', icon: Minimize2 },
      { id: 'extract', label: '提取', icon: FileJson },
    ],
  },
  {
    label: '安全',
    icon: ShieldCheck,
    items: [
      { id: 'permissions', label: '权限', icon: Lock },
      { id: 'audit', label: '审计', icon: ShieldCheck },
    ],
  },
  {
    label: '运行时',
    icon: Activity,
    items: [
      { id: 'workflow', label: '工作流', icon: GitBranch },
      { id: 'sandbox', label: '沙箱', icon: Terminal },
      { id: 'scheduler', label: '定时任务', icon: Timer },
    ],
  },
  {
    label: '开发',
    icon: GitFork,
    items: [
      { id: 'worktree', label: '工作树', icon: GitFork },
    ],
  },
  {
    label: '智能',
    icon: Sparkles,
    items: [
      { id: 'evolution', label: '自进化', icon: Sparkles },
    ],
  },
];

const panels: Record<SettingsTabId, React.FC> = {
  tools: ToolsPanel,
  mcp: McpPanel,
  skills: SkillsPanel,
  memory: MemoryPanel,
  'auto-memory': AutoMemoryPanel,
  config: ConfigPanel,
  providers: ProviderPanel,
  sessions: SessionsPanel,
  permissions: PermissionsPanel,
  audit: AuditPanel,
  workflow: WorkflowPanel,
  sandbox: SandboxPanel,
  compress: CompressPanel,
  extract: ExtractPanel,
  evolution: EvolutionPanel,
  plugins: PluginPanel,
  scheduler: SchedulerPanel,
  worktree: WorktreePanel,
};

export function SettingsDialog() {
  const { settingsOpen, closeSettings, activeSettingsTab, setActiveSettingsTab } = useUiStore();
  const Panel = panels[activeSettingsTab];

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

  const activeItem = settingsGroups.flatMap(g => g.items).find(i => i.id === activeSettingsTab);

  return (
    <>
      {/* Backdrop */}
      <div className="fixed inset-0 z-50" style={{ background: 'var(--bg-overlay)' }} onClick={closeSettings} />

      {/* Dialog */}
      <div className="animate-scale-in fixed left-1/2 top-1/2 z-50 flex h-[85vh] w-[92vw] max-w-6xl -translate-x-1/2 -translate-y-1/2 overflow-hidden rounded-2xl border border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-2xl">
        {/* Left sidebar — settings nav */}
        <div className="flex w-[220px] shrink-0 flex-col border-r border-[var(--border-primary)] bg-[var(--bg-sidebar)]">
          <div className="flex items-center justify-between border-b border-[var(--border-primary)] px-5 py-4">
            <h2 className="text-sm font-semibold tracking-tight text-[var(--text-primary)]">设置</h2>
            <button
              onClick={closeSettings}
              className="flex h-7 w-7 items-center justify-center rounded-lg text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
            >
              <X size={15} />
            </button>
          </div>
          <nav className="flex-1 overflow-y-auto p-3 space-y-5">
            {settingsGroups.map((group) => (
              <div key={group.label}>
                {/* Group header */}
                <div className="flex items-center gap-2 px-2.5 pb-2">
                  <group.icon size={12} className="text-[var(--text-tertiary)]" />
                  <span className="text-[11px] font-semibold uppercase tracking-widest text-[var(--text-tertiary)]">
                    {group.label}
                  </span>
                </div>
                {group.items.map(({ id, label, icon: Icon }) => (
                  <button
                    key={id}
                    onClick={() => setActiveSettingsTab(id)}
                    className={`flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-[13px] font-medium transition-all duration-150
                      ${activeSettingsTab === id
                        ? 'bg-[var(--accent)]/10 text-[var(--accent)] shadow-sm'
                        : 'text-[var(--text-secondary)] hover:bg-[var(--bg-sidebar-hover)] hover:text-[var(--text-primary)]'
                      }`}
                  >
                    <Icon size={15} className="shrink-0" />
                    {label}
                  </button>
                ))}
              </div>
            ))}
          </nav>
        </div>

        {/* Content area */}
        <div className="flex flex-1 flex-col min-w-0">
          <div className="flex items-center justify-between border-b border-[var(--border-primary)] px-6 py-3.5">
            <span className="text-sm font-semibold text-[var(--text-primary)]">
              {activeItem?.label}
            </span>
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
