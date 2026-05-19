import { Sparkles } from 'lucide-react';

interface BrandIconProps {
  size?: 'sm' | 'md' | 'lg';
}

const sizeMap = {
  sm: { container: 'h-7 w-7', icon: 12 },
  md: { container: 'h-8 w-8', icon: 14 },
  lg: { container: 'h-12 w-12', icon: 22 },
};

export function BrandIcon({ size = 'md' }: BrandIconProps) {
  const s = sizeMap[size];
  return (
    <div
      className={`${s.container} flex shrink-0 items-center justify-center rounded-xl`}
      style={{
        background: 'var(--accent)',
      }}
    >
      <Sparkles size={s.icon} color="white" />
    </div>
  );
}
