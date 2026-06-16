export const PERMISSIONS_CHANGED_EVENT = 'eko:permissions-changed';

export const PERMISSION_MODES = [
  { id: 'default', label: '默认审批', description: '高风险操作会询问' },
  { id: 'plan', label: '计划模式', description: '先规划，再确认执行' },
  { id: 'auto-edit', label: '自动编辑', description: '编辑类操作自动通过' },
  { id: 'full-auto', label: '全自动', description: '尽量不打断执行' },
  { id: 'dontask', label: '严格确认', description: '敏感操作都询问' },
] as const;

export function notifyPermissionsChanged() {
  window.dispatchEvent(new Event(PERMISSIONS_CHANGED_EVENT));
}

export function normalizePermissionMode(mode?: string) {
  switch (mode) {
    case 'autoedit':
    case 'accept-edits':
      return 'auto-edit';
    case 'fullauto':
    case 'bypass':
      return 'full-auto';
    case 'dont-ask':
    case 'strict':
      return 'dontask';
    case 'ask':
      return 'default';
    default:
      return mode || 'default';
  }
}
