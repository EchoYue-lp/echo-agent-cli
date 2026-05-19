interface StatusBadgeProps {
  status: 'success' | 'error' | 'warning' | 'info';
  label: string;
  size?: 'sm' | 'md';
}

export function StatusBadge({ status, label, size = 'md' }: StatusBadgeProps) {
  return (
    <span
      className="inline-flex items-center rounded-full font-medium"
      style={{
        padding: size === 'sm' ? '1px 6px' : '2px 10px',
        fontSize: size === 'sm' ? '10px' : '11px',
        background: `var(--color-${status}-bg)`,
        color: `var(--color-${status})`,
      }}
    >
      {label}
    </span>
  );
}
