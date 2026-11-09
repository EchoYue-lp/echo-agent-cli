import { CircleCheck, CircleX, Info, TriangleAlert } from 'lucide-react';

interface StatusBadgeProps {
  status: 'success' | 'error' | 'warning' | 'info';
  label: string;
  size?: 'sm' | 'md';
}

const statusIcons = { success: CircleCheck, error: CircleX, warning: TriangleAlert, info: Info };

export function StatusBadge({ status, label, size = 'md' }: StatusBadgeProps) {
  const Icon = statusIcons[status];
  return (
    <span
      className="inline-flex items-center gap-1 rounded-full font-medium leading-5"
      style={{
        padding: size === 'sm' ? '1px 7px' : '2px 9px',
        fontSize: size === 'sm' ? '11px' : '12px',
        background: `var(--color-${status}-bg)`,
        color: `var(--color-${status}-text)`,
      }}
    >
      <Icon size={size === 'sm' ? 11 : 12} aria-hidden="true" />
      {label}
    </span>
  );
}
