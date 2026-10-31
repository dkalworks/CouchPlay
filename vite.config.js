import { defineConfig } from 'vite';
import { resolve } from 'node:path';

const root = resolve(import.meta.dirname, 'client');

export default defineConfig({
  root,
  envDir: import.meta.dirname,
  server: {
    port: 5173,
    // Allow tunnel hostnames (cloudflared etc.) used for Discord testing.
    allowedHosts: true,
    proxy: {
      '/api': { target: 'http://localhost:3001', ws: true },
    },
  },
  build: {
    outDir: resolve(import.meta.dirname, 'dist'),
    emptyOutDir: true,
    rollupOptions: {
      input: {
        viewer: resolve(root, 'index.html'),
        host: resolve(root, 'host.html'),
      },
    },
  },
});
