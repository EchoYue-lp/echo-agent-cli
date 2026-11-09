import { BrandIcon } from '../common/BrandIcon';

export function WelcomeScreen() {
  return (
    <div className="mx-auto flex w-full max-w-[980px] flex-col items-center px-6 pb-7 pt-8 text-center">
      <div className="mb-2 flex flex-col items-center">
        <div className="animate-slide-up mb-4">
          <BrandIcon size="lg" />
        </div>
        <h1
          className="animate-slide-up text-[clamp(28px,3.2vw,40px)] font-semibold tracking-[-0.04em] text-[var(--text-primary)]"
          style={{ animationDelay: '0.05s' }}
        >
          把想法变成成果
        </h1>
        <p className="mt-1 text-sm text-[var(--text-tertiary)]" aria-label="品牌读音">
          Eclip /ˈɛk.lɪp/
        </p>
        <p
          className="animate-slide-up mt-2 text-sm text-[var(--text-secondary)]"
          style={{ animationDelay: '0.1s' }}
        >
          听见你的声音，让想法飞跃。
        </p>
      </div>
    </div>
  );
}
