import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';

export default defineConfig(({ mode }) => {
  const isTauriMode = mode === 'tauri';

  return {
    plugins: [react(), tailwindcss()],
    build: {
      // Tauri v2 frontend plugins must be bundled by Vite — they are NOT
      // available as global scripts in the webview. Do NOT externalize them.
      // Keep a bounded warning threshold after large editor, terminal, markdown,
      // and icon dependencies are split into independently cached chunks.
      chunkSizeWarningLimit: 700,
      rollupOptions: {
        output: {
          manualChunks: {
            // 实际入口使用 client；将 renderer 与既有 React vendor 一起缓存。
            react: ['react', 'react-dom', 'react-dom/client'],
            editor: ['@uiw/react-codemirror', '@codemirror/language-data'],
            markdown: ['react-markdown', 'remark-gfm'],
            terminal: ['@xterm/xterm', '@xterm/addon-fit'],
            icons: ['lucide-react'],
            state: ['zustand'],
            // 图表按需载入；解释运行时和编译器各自成块，保留既有包体阈值。
            'chart-runtime': ['vega', 'vega-interpreter'],
            'chart-compiler': ['vega-lite'],
            'chart-embed': ['vega-embed'],
          },
        },
      },
    },
    server: {
      port: 1420,
      strictPort: true,
      proxy: isTauriMode
        ? undefined
        : {
            '/api': 'http://localhost:3000',
            '/ws': { target: 'ws://localhost:3000', ws: true },
          },
    },
  };
});
