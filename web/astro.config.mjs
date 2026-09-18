// @ts-check
import { defineConfig } from 'astro/config';
import node from '@astrojs/node';

import svelte from '@astrojs/svelte';
import tailwindcss from '@tailwindcss/vite';

// The board is server-rendered so alarm state is correct on first paint, with
// the live list hydrated as an island fed by the daemon's SSE stream.
// `oarfish-api` proxies /api/* to the Rust daemon in production; in dev the
// daemon is expected on :4000.
export default defineConfig({
  output: 'server',
  adapter: node({ mode: 'standalone' }),
  server: { port: 4321 },

  vite: {
    server: {
      proxy: {
        '/api': { target: 'http://127.0.0.1:4000', changeOrigin: true },
      },
    },

    plugins: [tailwindcss()],
  },

  integrations: [svelte()],
});