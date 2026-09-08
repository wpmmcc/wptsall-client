import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { viteSingleFile } from 'vite-plugin-singlefile';
import tailwindcss from '@tailwindcss/vite';

export default defineConfig({
  plugins: [tailwindcss(), svelte(), viteSingleFile()],
  server: {
    port: 5173,
    proxy: {
      '/api': 'http://127.0.0.1:8977',
      '/oauth': 'http://127.0.0.1:8977',
    }
  },
  build: {
    assetsInlineLimit: Infinity,
    cssCodeSplit: false,
    target: 'es2015',
  }
});
