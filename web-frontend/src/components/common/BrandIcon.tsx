interface BrandIconProps {
  size?: 'sm' | 'md' | 'lg';
}

const sizeMap = {
  sm: { container: 'h-7 w-7', icon: 17 },
  md: { container: 'h-8 w-8', icon: 19 },
  lg: { container: 'h-12 w-12', icon: 28 },
};

export function BrandIcon({ size = 'md' }: BrandIconProps) {
  const s = sizeMap[size];
  return (
    <div
      className={`${s.container} flex shrink-0 items-center justify-center rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] text-[var(--text-primary)]`}
      style={{
        boxShadow: 'var(--shadow-sm)',
      }}
    >
      <svg width={s.icon} height={s.icon} viewBox="0 0 32 32" fill="none" aria-hidden="true">
        <path
          d="M8.5 9.5h15v13h-15z"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinejoin="round"
        />
        <path d="M12 14h8" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" />
        <path d="M12 18h5" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" />
        <circle cx="21.5" cy="18" r="1.8" fill="currentColor" />
      </svg>
    </div>
  );
}
