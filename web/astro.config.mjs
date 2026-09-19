// @ts-check
import { defineConfig } from 'astro/config';

import svelte from '@astrojs/svelte';
import tailwindcss from '@tailwindcss/vite';

// A static build: the daemon serves it directly over `tower-http::ServeDir`
// via `--static-dir`, one binary and one port in production. The static
// shell paints instantly; `AlarmList` and `ConnectionPulse` fetch
// `/api/alarms` and open the SSE stream themselves once mounted. In dev,
// `astro dev` proxies `/api` to the daemon expected on :4000.
export default defineConfig({
  output: 'static',
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