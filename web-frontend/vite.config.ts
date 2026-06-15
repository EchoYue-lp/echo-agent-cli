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
