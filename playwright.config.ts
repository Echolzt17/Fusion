import { defineConfig } from '@playwright/test';

const port = Number(process.env.FUSION_UI_TEST_PORT || 15379);
const baseURL = `http://127.0.0.1:${port}`;

export default defineConfig({
  testDir: './tests/ui',
  workers: 1,
  timeout: 30_000,
  use: { baseURL, channel: 'msedge', headless: true, viewport: { width: 1080, height: 760 } },
  // Exercise the same eagerly bundled assets shipped in the desktop app. A Vite
  // dev HTML 200 does not mean its lazy transforms/dependency graph are ready.
  webServer: { command: `npm run web:build && npx vite preview --host 127.0.0.1 --port ${port} --strictPort`, url: baseURL, reuseExistingServer: false },
});
