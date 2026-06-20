/**
 * Playwright config for the RD console e2e (#13, v0.4 Director wiring).
 *
 * One spec drives the **full RD click-through in real headless chromium** — log in, define
 * a heat with named pilots, run it, watch the live laps climb in the rendered DOM, finish,
 * score, read results — against a **real** Director (`gridfpv` binary, built-in sim source).
 *
 * `webServer` stands the whole stack up for the run, in order, on a fixed port:
 *   1. `npm run build` the console (and its workspace deps) so `dist/` matches the source;
 *   2. launch the Director with a *known* RD token (`GRIDFPV_RD_TOKEN`) and a fast sim
 *      (4 laps @ 250ms) serving that `dist/` as its SPA (`GRIDFPV_ASSETS`).
 * The Director serves the SPA at its own origin, so the spec navigates straight at it —
 * one origin, the real control path, the real sim laps. Playwright waits on `/health`.
 *
 * Headless chromium only: the console is the dense desktop surface; this proves the
 * click-through and the live render path, not cross-browser rendering.
 */
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig, devices } from '@playwright/test';

const here = fileURLToPath(new URL('.', import.meta.url));
const frontendRoot = here;
const repoRoot = resolve(frontendRoot, '..');
const dist = resolve(frontendRoot, 'apps', 'rd-console', 'dist');
const director = resolve(repoRoot, 'target', 'debug', 'gridfpv');

// A fixed, loopback-only port + the known RD token the spec logs in with.
const PORT = 8123;
const BASE_URL = `http://127.0.0.1:${PORT}`;
export const RD_TOKEN = 'test-rd-token';

export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.spec.ts',
  // A real sim race takes a few seconds across its phases; give generous headroom.
  timeout: 90_000,
  expect: { timeout: 15_000 },
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: process.env.CI ? 'line' : 'list',
  use: {
    baseURL: BASE_URL,
    headless: true,
    trace: 'retain-on-failure'
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    // Build the console — `npm run build` builds every workspace, so the rd-console SPA
    // gets its freshly built `@gridfpv/*` deps — then exec the real Director on $PORT,
    // serving that SPA with a known token and a fast sim. `exec` replaces the shell with the
    // Director so Playwright's process tree can stop it cleanly.
    command: `npm run build && exec ${JSON.stringify(director)}`,
    cwd: frontendRoot,
    url: `${BASE_URL}/health`,
    timeout: 180_000,
    reuseExistingServer: !process.env.CI,
    stdout: 'pipe',
    stderr: 'pipe',
    env: {
      GRIDFPV_ADDR: `127.0.0.1:${PORT}`,
      GRIDFPV_RD_TOKEN: RD_TOKEN,
      GRIDFPV_SIM_LAPS: '4',
      GRIDFPV_SIM_LAP_MS: '250',
      GRIDFPV_ASSETS: dist
    }
  }
});
