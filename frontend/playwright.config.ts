/**
 * Playwright config for the RD console e2e (#13, v0.4 Director wiring + observability harness).
 *
 * The specs drive the **full RD click-through in real headless chromium** — log in, define a heat
 * with named pilots, run it, watch the live laps climb in the rendered DOM, finish, score, read
 * results — against a **real** Director (`gridfpv` binary, built-in sim source).
 *
 * The Director is no longer stood up by a `webServer` here; it is a **worker-scoped fixture** in
 * `e2e/observability.ts` (backed by the reusable `test-harness/director.ts` boot harness), so the
 * same boot path is shared with the Node-only contract suite and so every spec automatically taps
 * the Director's server log into its on-failure dump. We only need to ensure the SPA `dist/` is
 * built before the run — that is this config's `globalSetup`.
 *
 * Headless chromium only: the console is the dense desktop surface; this proves the click-through
 * and the live render path, not cross-browser rendering.
 */
import { defineConfig, devices } from '@playwright/test';

// The known RD token the observability fixture pins on the Director and the specs log in with.
export const RD_TOKEN = 'test-rd-token';

export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.spec.ts',
  // Build the SPA the worker-scoped Director will serve, before any worker boots one.
  globalSetup: './e2e/global-setup.ts',
  // A real sim race takes a few seconds across its phases; give generous headroom.
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
