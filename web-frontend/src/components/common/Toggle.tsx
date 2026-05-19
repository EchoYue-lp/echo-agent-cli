interface ToggleProps {
  checked: boolean;
  onChange: (v: boolean) => void;
  label?: string;
  disabled?: boolean;
}

export function Toggle({ checked, onChange, label, disabled = false }: ToggleProps) {
  return (
    <button
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className="relative inline-flex shrink-0 cursor-pointer rounded-full transition-colors duration-200"
      style={{
        width: 36,
        height: 20,
        background: checked ? 'var(--accent)' : 'var(--text-tertiary)',
        opacity: disabled ? 0.5 : 1,
      }}
      title={label}
    >
      {label && <span className="sr-only">{label}</span>}
      <span
        className="absolute top-0.5 rounded-full bg-white shadow-sm transition-transform duration-200"
        style={{
          width: 16,
          height: 16,
          left: 2,
          transform: checked ? 'translateX(16px)' : 'translateX(0)',
        }}
      />
    </button>
  );
}
