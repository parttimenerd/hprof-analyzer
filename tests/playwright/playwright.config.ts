import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  timeout: 60_000,
  retries: 0,
  use: {
    headless: true,
    viewport: { width: 1280, height: 900 },
  },
  // 'line' for readable terminal output; 'html' for the CI artifact.
  reporter: process.env.CI ? [['line'], ['html', { open: 'never' }]] : [['line']],
});
