/** Transient per-view I/O budgets, not terminal history or a replay store. */
export const OUTPUT_QUEUE_BYTES = 1024 * 1024;
export const INPUT_QUEUE_BYTES = 64 * 1024;
const MAX_QUEUE_CHUNKS = 256;

export function decodeTerminalBytes(base64: string): Uint8Array {
  const binary = atob(base64);
  return Uint8Array.from(binary, (character) => character.charCodeAt(0));
}

export function encodeTerminalBytes(bytes: Uint8Array): string {
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

export function boundedTerminalQueue(
  write: (bytes: Uint8Array) => Promise<void>,
  fail: (message: string) => void,
  limit: number,
  overflowMessage: string
) {
  const pending: Uint8Array[] = [];
  let inFlight = false;
  let activeBytes = 0;
  let heldBytes = 0;
  let failed = false;
  let disposed = false;
  const stop = (message: string) => {
    if (failed || disposed) return;
    failed = true;
    pending.length = 0;
    heldBytes = activeBytes;
    fail(message);
  };
  const drain = () => {
    if (disposed || failed || inFlight) return;
    const bytes = pending.shift();
    if (!bytes) return;
    inFlight = true;
    activeBytes = bytes.byteLength;
    void Promise.resolve()
      .then(() => {
        if (!disposed && !failed) return write(bytes);
      })
      .then(
        () => {
          inFlight = false;
          activeBytes = 0;
          heldBytes = Math.max(0, heldBytes - bytes.byteLength);
          drain();
        },
        () => {
          // No input content in the diagnostic, and no automatic resend: part of
          // an attempted write may already have reached the original process.
          inFlight = false;
          activeBytes = 0;
          heldBytes = 0;
          stop('终端写入失败；部分输入或输出可能已处理，不自动重放。');
        }
      );
  };
  return {
    push(bytes: Uint8Array): boolean {
      if (disposed || failed) return false;
      if (bytes.byteLength > limit - heldBytes || pending.length >= MAX_QUEUE_CHUNKS) {
        stop(overflowMessage);
        return false;
      }
      heldBytes += bytes.byteLength;
      pending.push(bytes);
      drain();
      return true;
    },
    dispose() {
      disposed = true;
      pending.length = 0;
      heldBytes = 0;
    },
    pendingBytes: () => heldBytes,
  };
}

/** Bound only observation; the original native Promise keeps running. */
export async function observeTerminal<T>(original: Promise<T>): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      original,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error('终端结算等待超时；原创建/关闭仍在继续，可重试观察。')),
          5000
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}
