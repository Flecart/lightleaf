import { defineConfig } from 'vite';
import preact from '@preact/preset-vite';

const backend = process.env.LIGHTLEAF_BACKEND ?? 'http://127.0.0.1:8080';

export default defineConfig({
  plugins: [preact()],
  build: {
    target: 'es2022',
    chunkSizeWarningLimit: 1500,
    rollupOptions: {
      output: {
        manualChunks: {
          pdfjs: ['pdfjs-dist'],
          codemirror: ['@codemirror/view', '@codemirror/state', '@codemirror/language', '@codemirror/commands'],
        },
      },
    },
  },
  server: {
    proxy: {
      '/api': { target: backend, ws: true, changeOrigin: false },
      '/git': { target: backend },
    },
  },
});
