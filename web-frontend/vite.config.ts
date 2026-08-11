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
            react: ['react', 'react-dom'],
            editor: ['@uiw/react-codemirror', '@codemirror/language-data'],
            markdown: ['react-markdown', 'remark-gfm'],
            terminal: ['@xterm/xterm', '@xterm/addon-fit'],
            icons: ['lucide-react'],
            state: ['zustand'],
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
