export function Skeleton({
  className = '',
  width,
  height = 16,
}: {
  className?: string;
  width?: number | string;
  height?: number;
}) {
  return (
    <div
      className={`rounded-md animate-pulse ${className}`}
      style={{
        background: 'var(--bg-hover)',
        width: width ?? '100%',
        height,
      }}
    />
  );
}

export function CardSkeleton() {
  return (
    <div
      className="rounded-lg border p-3 space-y-2"
      style={{ borderColor: 'var(--border-primary)', background: 'var(--bg-primary)' }}
    >
      <div className="flex items-center gap-2">
        <Skeleton width={14} height={14} />
        <Skeleton width="40%" height={14} />
      </div>
      <Skeleton width="80%" height={12} />
      <Skeleton width="60%" height={12} />
    </div>
  );
}

export function PanelSkeleton({ rows = 4 }: { rows?: number }) {
  return (
    <div className="p-3 space-y-3">
      <Skeleton width="30%" height={18} />
      {Array.from({ length: rows }).map((_, i) => (
        <CardSkeleton key={i} />
      ))}
    </div>
  );
}
