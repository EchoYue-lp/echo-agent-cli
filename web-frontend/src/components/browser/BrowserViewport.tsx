import { useEffect, useRef } from 'react';
import { Globe2 } from 'lucide-react';

export function imagePoint(
  clientX: number,
  clientY: number,
  rect: Pick<DOMRect, 'left' | 'top' | 'width' | 'height'>,
  naturalWidth: number,
  naturalHeight: number
): { x: number; y: number } | null {
  if (rect.width <= 0 || rect.height <= 0 || naturalWidth <= 0 || naturalHeight <= 0) return null;
  const scale = Math.min(rect.width / naturalWidth, rect.height / naturalHeight);
  const renderedWidth = naturalWidth * scale;
  const renderedHeight = naturalHeight * scale;
  const renderedLeft = rect.left + (rect.width - renderedWidth) / 2;
  const renderedTop = rect.top;
  if (
    clientX < renderedLeft ||
    clientX > renderedLeft + renderedWidth ||
    clientY < renderedTop ||
    clientY > renderedTop + renderedHeight
  ) {
    return null;
  }
  const x = (clientX - renderedLeft) / scale;
  const y = (clientY - renderedTop) / scale;
  return { x, y };
}

export function BrowserViewport({
  frame,
  busy,
  clickable,
  scrollable,
  onClickAt,
  onScroll,
}: {
  frame?: string | null;
  busy: boolean;
  clickable: boolean;
  scrollable: boolean;
  onClickAt: (x: number, y: number) => void;
  onScroll: (deltaX: number, deltaY: number) => void;
}) {
  const scrollTimer = useRef<number | null>(null);
  const pendingScroll = useRef({ x: 0, y: 0 });
  useEffect(
    () => () => {
      if (scrollTimer.current !== null) window.clearTimeout(scrollTimer.current);
    },
    []
  );
  return (
    <div
      className="relative min-h-0 flex-1 overflow-hidden bg-white"
      onWheel={(event) => {
        if (!scrollable || busy || !frame) return;
        event.preventDefault();
        if (scrollTimer.current !== null) window.clearTimeout(scrollTimer.current);
        pendingScroll.current.x += event.deltaX;
        pendingScroll.current.y += event.deltaY;
        scrollTimer.current = window.setTimeout(() => {
          const { x, y } = pendingScroll.current;
          pendingScroll.current = { x: 0, y: 0 };
          scrollTimer.current = null;
          onScroll(x, y);
        }, 50);
      }}
    >
      {frame ? (
        <button
          type="button"
          aria-label="在浏览器页面中点击"
          disabled={!clickable || busy}
          onClick={(event) => {
            const image = event.currentTarget.querySelector('img');
            if (!image) return;
            const point = imagePoint(
              event.clientX,
              event.clientY,
              image.getBoundingClientRect(),
              image.naturalWidth,
              image.naturalHeight
            );
            if (point) onClickAt(point.x, point.y);
          }}
          className={`block h-full w-full select-none border-0 bg-transparent p-0 ${clickable && !busy ? 'cursor-pointer' : 'cursor-default'}`}
        >
          <img
            src={frame}
            alt="浏览器页面截图"
            draggable={false}
            className="block h-full w-full object-contain object-top"
          />
        </button>
      ) : (
        <div className="flex h-full min-h-[240px] flex-col items-center justify-center gap-2 bg-[var(--bg-chat)] text-[var(--text-tertiary)]">
          <Globe2 size={28} strokeWidth={1.4} />
          <span className="text-xs">浏览器画面将在操作后显示</span>
        </div>
      )}
      {busy && (
        <div className="pointer-events-none absolute inset-x-0 top-0 h-0.5 overflow-hidden bg-[var(--border-primary)]">
          <div className="h-full w-1/3 animate-pulse bg-[var(--accent)]" />
        </div>
      )}
    </div>
  );
}
