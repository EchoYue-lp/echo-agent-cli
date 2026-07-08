import {
  Code,
  BarChart3,
  GraduationCap,
  HeartPulse,
  MoreHorizontal,
  type LucideIcon,
} from 'lucide-react';

/**
 * Workspace kind → visual identity (icon + color + label).
 *
 * Single source of truth shared by LeftSidebar and NewTaskDialog. Previously
 * each file hand-rolled its own map with slightly different coverage
 * (LeftSidebar had 4 kinds, NewTaskDialog had 5) and duplicated the same
 * hex colors. Colors here are semantic kind identifiers (not theme chrome),
 * so they stay as literal hex rather than CSS tokens — a "code" workspace is
 * always indigo regardless of light/dark.
 */
export interface WorkspaceKindMeta {
  value: string;
  label: string;
  icon: LucideIcon;
  desc: string;
  color: string;
}

export const WORKSPACE_KINDS: WorkspaceKindMeta[] = [
  {
    value: 'code',
    label: '代码项目',
    icon: Code,
    desc: '自动激活 coding、git-workflow 技能',
    color: '#6366f1',
  },
  {
    value: 'data',
    label: '数据分析',
    icon: BarChart3,
    desc: '自动激活 data-wrangling、statistical-analysis、data-visualization 技能',
    color: '#f59e0b',
  },
  {
    value: 'data_analysis',
    label: '数据分析',
    icon: BarChart3,
    desc: '自动激活 data-wrangling、statistical-analysis、data-visualization 技能',
    color: '#f59e0b',
  },
  {
    value: 'research',
    label: '学术研究',
    icon: GraduationCap,
    desc: '自动激活 paper-search、paper-reader、doc-writing 技能',
    color: '#10b981',
  },
  {
    value: 'medical',
    label: '医学研究',
    icon: HeartPulse,
    desc: '自动激活 evidence-medicine、paper-search、paper-reader 技能',
    color: '#ef4444',
  },
  {
    value: 'general',
    label: '其他',
    icon: MoreHorizontal,
    desc: '不自动激活特定技能，所有工具可用',
    color: '#6b7280',
  },
];

const KIND_MAP: Record<string, WorkspaceKindMeta> = Object.fromEntries(
  WORKSPACE_KINDS.map((k) => [k.value, k])
);

const DEFAULT_KIND = KIND_MAP['general'];

/** Resolve a kind string (from backend) to its visual meta, falling back to general. */
export function getWorkspaceKind(kind: string): WorkspaceKindMeta {
  return KIND_MAP[kind] ?? DEFAULT_KIND;
}
