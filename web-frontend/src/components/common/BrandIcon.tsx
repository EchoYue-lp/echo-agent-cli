interface BrandIconProps {
  size?: 'sm' | 'md' | 'lg';
}

const sizeMap = {
  sm: { className: 'h-7 w-7', pixels: 28 },
  md: { className: 'h-8 w-8', pixels: 32 },
  lg: { className: 'h-12 w-12', pixels: 48 },
};

export function BrandIcon({ size = 'md' }: BrandIconProps) {
  const s = sizeMap[size];
  return (
    <img
      src="/app-icon.svg"
      alt=""
      aria-hidden="true"
      width={s.pixels}
      height={s.pixels}
      className={`${s.className} shrink-0`}
    />
  );
}
