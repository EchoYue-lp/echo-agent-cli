export interface ThinkingLevelOption {
  id: string;
  label: string;
}

const LEVEL_LABELS: Record<string, string> = {
  none: '关闭',
  minimal: '最低',
  low: '低',
  medium: '中',
  high: '高',
  xhigh: '很高',
  max: '最高',
};

export function isKnownThinkingLevel(value: string): boolean {
  return value === 'auto' || Boolean(LEVEL_LABELS[value]);
}

export function thinkingLevelOptions(levels: readonly string[]): ThinkingLevelOption[] {
  return [
    { id: 'auto', label: '自动' },
    ...levels.map((id) => ({ id, label: LEVEL_LABELS[id] ?? id })),
  ];
}
