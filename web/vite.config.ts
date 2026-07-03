import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

// Build output goes to web/dist. Point the server at it with
// QE_SERVER_STATIC_DIR=web/dist (see QE-254 static-SPA serving).
export default defineConfig({
  plugins: [react()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    // Local dev: proxy /api to the qe-server so cookie auth is same-origin.
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:8080',
        changeOrigin: true,
      },
    },
  },
  test: {
    globals: true,
    environment: 'jsdom',
    setupFiles: ['./vitest.setup.ts'],
    css: false,
    include: ['src/**/*.{test,spec}.{ts,tsx}'],
  },
});
