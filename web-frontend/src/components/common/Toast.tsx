import { useToastStore } from '../../stores/toastStore';
import { X, CheckCircle, AlertCircle, AlertTriangle, Info } from 'lucide-react';

const iconMap = {
  success: CheckCircle,
  error: AlertCircle,
  warning: AlertTriangle,
  info: Info,
};

const colorMap = {
  success: {
    bg: 'var(--color-success-bg)',
    border: 'var(--color-success)',
    text: 'var(--color-success)',
    icon: 'var(--color-success)',
  },
  error: {
    bg: 'var(--color-error-bg)',
    border: 'var(--color-error)',
    text: 'var(--color-error)',
    icon: 'var(--color-error)',
  },
  warning: {
    bg: 'var(--color-warning-bg)',
    border: 'var(--color-warning)',
    text: 'var(--color-warning)',
    icon: 'var(--color-warning)',
  },
  info: {
    bg: 'var(--color-info-bg)',
    border: 'var(--color-info)',
    text: 'var(--color-info)',
    icon: 'var(--color-info)',
  },
};

export function ToastContainer() {
  const toasts = useToastStore((s) => s.toasts);
  const removeToast = useToastStore((s) => s.removeToast);

  if (toasts.length === 0) return null;

  return (
    <div className="fixed bottom-4 right-4 z-50 flex flex-col gap-2 max-w-sm">
      {toasts.map((toast) => {
        const Icon = iconMap[toast.type];
        const colors = colorMap[toast.type];
        return (
          <div
            key={toast.id}
            className="flex items-start gap-2 rounded-lg border px-3 py-2.5 shadow-[var(--shadow-lg)] animate-fade-up"
            style={{ background: colors.bg, borderColor: colors.border }}
          >
            <Icon size={16} style={{ color: colors.icon, flexShrink: 0 }} />
            <p className="flex-1 text-xs leading-relaxed" style={{ color: colors.text }}>
              {toast.message}
            </p>
            <button
              onClick={() => removeToast(toast.id)}
              style={{ color: colors.text, flexShrink: 0 }}
            >
              <X size={14} />
            </button>
          </div>
        );
      })}
    </div>
  );
}
