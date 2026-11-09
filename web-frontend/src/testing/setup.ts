const domWindow = (globalThis as typeof globalThis & { jsdom?: { window: Window } }).jsdom?.window;

if (domWindow) {
  // Node 的同名属性可能存在但不可用；浏览器测试必须使用本文件自己的真实 jsdom Storage。
  // setupFiles 先于测试模块和 hoisted 初始化执行，node-only 环境则保持宿主原样。
  for (const key of ['localStorage', 'sessionStorage'] as const) {
    Object.defineProperty(globalThis, key, {
      configurable: true,
      get: () => domWindow[key],
    });
  }
}
