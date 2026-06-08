interface BrandIconProps {
  size?: 'sm' | 'md' | 'lg';
}

const sizeMap = {
  sm: { container: 'h-7 w-7', icon: 19 },
  md: { container: 'h-8 w-8', icon: 21 },
  lg: { container: 'h-12 w-12', icon: 32 },
};

export function BrandIcon({ size = 'md' }: BrandIconProps) {
  const s = sizeMap[size];
  return (
    <div
      className={`${s.container} flex shrink-0 items-center justify-center rounded-xl`}
      style={{
        background: 'linear-gradient(135deg, #14b8a6 0%, #0ea5e9 55%, #6366f1 100%)',
        boxShadow: 'inset 0 1px 0 rgb(255 255 255 / 0.22)',
      }}
    >
      <svg
        width={s.icon}
        height={s.icon}
        viewBox="0 0 32 32"
        fill="none"
        aria-hidden="true"
      >
        <path
          d="M9.9 11.9c-2 1.6-3.2 4-3.2 6.8s1.2 5.2 3.2 6.8"
          stroke="white"
          strokeWidth="2.1"
          strokeLinecap="round"
          opacity="0.68"
        />
        <path
          d="M11.8 14.2c-1.1 1-1.8 2.4-1.8 4s.7 3 1.8 4"
          stroke="white"
          strokeWidth="2"
          strokeLinecap="round"
          opacity="0.9"
        />
        <path
          d="M16 10.6c-3.7 0-6.7 2.8-6.7 6.2 0 2.1 1.1 3.9 2.8 5l-.8 2.7 3-1.5c.6.1 1.1.2 1.8.2 3.7 0 6.7-2.8 6.7-6.2s-3.1-6.4-6.8-6.4Z"
          fill="white"
        />
        <path d="M13.4 16.9h5.2" stroke="#0ea5e9" strokeWidth="2" strokeLinecap="round" />
        <path d="M16 14.3v5.2" stroke="#14b8a6" strokeWidth="2" strokeLinecap="round" />
        <circle cx="16" cy="16.9" r="1" fill="#eef9ff" />
      </svg>
    </div>
  );
}
