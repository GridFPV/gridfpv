/**
 * Playwright config for the RD console e2e.
 *
 * The specs drive the console in real headless chromium — the hub, the Events, Pilots and
 * Classes pages, and an open event — against a **real** Director (the `gridfpv` binary).
 *
 * The Director is a **worker-scoped fixture** in `e2e/observability.ts` (backed by the reusable
 * `test-harness/director.ts` boot harness), so the same boot path is shared with the Node-only
 * contract suite and every spec automatically taps the Director's server log into its
 * on-failure dump. We only need to ensure the SPA `dist/` is built before the run — that is
 * this config's `globalSetup`.
 *
 * Headless chromium only: the console is the dense desktop surface; this proves the click-through
 * and the render path, not cross-browser rendering.
 */
import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.spec.ts',
  // Build the SPA the worker-scoped Director will serve, before any worker boots one.
  globalSetup: './e2e/global-setup.ts',
  timeout: 90_000,
  expect: { timeout: 15_000 },
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: process.env.CI ? 'line' : 'list',
  use: {
    // baseURL is provided per-worker by the `director` fixture (its ephemeral URL).
    headless: true,
    trace: 'retain-on-failure'
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }]
});
