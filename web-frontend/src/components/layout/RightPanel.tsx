import { Wrench, Globe, BookOpen, Brain, Settings, ShieldCheck, GitBranch, Lock, Save, Terminal, Minimize2, FileJson } from 'lucide-react';
import { useUiStore, type TabId } from '../../stores/uiStore';
import { ToolsPanel } from '../tools/ToolsPanel';
import { McpPanel } from '../mcp/McpPanel';
import { SkillsPanel } from '../skills/SkillsPanel';
import { MemoryPanel } from '../memory/MemoryPanel';
import { ConfigPanel } from '../config/ConfigPanel';
import { AuditPanel } from '../audit/AuditPanel';
import { WorkflowPanel } from '../workflow/WorkflowPanel';
import { PermissionsPanel } from '../permissions/PermissionsPanel';
import { SessionsPanel } from '../sessions/SessionsPanel';
import { SandboxPanel } from '../sandbox/SandboxPanel';
import { CompressPanel } from '../compress/CompressPanel';
import { ExtractPanel } from '../extract/ExtractPanel';

const tabs: { id: TabId; label: string; icon: typeof Wrench }[] = [
  { id: 'tools', label: 'Tools', icon: Wrench },
  { id: 'mcp', label: 'MCP', icon: Globe },
  { id: 'skills', label: 'Skills', icon: BookOpen },
  { id: 'memory', label: 'Memory', icon: Brain },
  { id: 'config', label: 'Config', icon: Settings },
  { id: 'sessions', label: 'Sessions', icon: Save },
  { id: 'sandbox', label: 'Sandbox', icon: Terminal },
  { id: 'compress', label: 'Compress', icon: Minimize2 },
  { id: 'extract', label: 'Extract', icon: FileJson },
  { id: 'audit', label: 'Audit', icon: ShieldCheck },
  { id: 'workflow', label: 'Workflow', icon: GitBranch },
  { id: 'permissions', label: 'Permissions', icon: Lock },
];

const panels: Record<TabId, React.FC> = {
  tools: ToolsPanel,
  mcp: McpPanel,
  skills: SkillsPanel,
  memory: MemoryPanel,
  config: ConfigPanel,
  sessions: SessionsPanel,
  sandbox: SandboxPanel,
  compress: CompressPanel,
  extract: ExtractPanel,
  audit: AuditPanel,
  workflow: WorkflowPanel,
  permissions: PermissionsPanel,
};

export function RightPanel() {
  const { activeTab, setActiveTab } = useUiStore();
  const Panel = panels[activeTab];

  return (
    <div className="flex h-full flex-col" style={{ color: 'var(--text-primary)' }}>
      {/* Tab bar */}
      <div className="flex flex-wrap gap-0.5 px-2 py-1" style={{ borderBottom: '1px solid var(--border-primary)' }}>
        {tabs.map(({ id, label, icon: Icon }) => (
          <button
            key={id}
            onClick={() => setActiveTab(id)}
            className="flex items-center gap-1 rounded-md px-2 py-1 text-xs transition-colors"
            style={{
              background: activeTab === id ? 'var(--accent-bg)' : 'transparent',
              color: activeTab === id ? 'var(--accent)' : 'var(--text-secondary)',
              fontWeight: activeTab === id ? 500 : 400,
            }}
          >
            <Icon size={12} />
            {label}
          </button>
        ))}
      </div>

      {/* Panel content */}
      <div className="flex-1 overflow-y-auto">
        <Panel />
      </div>
    </div>
  );
}
