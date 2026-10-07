import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';
import path from 'path';

export default defineConfig({
  plugins: [svelte(), tailwindcss()],
  clearScreen: false,
  resolve: {
    alias: {
      '@webui': path.resolve(__dirname, '../../client-wpplugin/source/frontend/src'),
    },
  },
  server: {
    port: 1430,
    strictPort: true,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:9099',
        changeOrigin: true,
      },
    },
    fs: {
      allow: [
        path.resolve(__dirname, '.'),
        path.resolve(__dirname, '../../client-wpplugin/source/frontend'),
        path.resolve(__dirname, '../../client-wpplugin/source/locales'),
      ],
    },
  },
  envPrefix: ['VITE_', 'TAURI_'],
  build: {
    target: ['es2021', 'chrome100', 'safari14'],
    minify: !process.env.TAURI_DEBUG ? 'esbuild' : false,
    sourcemap: !!process.env.TAURI_DEBUG,
    outDir: 'dist',
  },
});
