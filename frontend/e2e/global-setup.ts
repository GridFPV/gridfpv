/**
 * Playwright global setup (#13, v0.4 observability harness).
 *
 * The worker-scoped `director` fixture serves the built RD console SPA (`GRIDFPV_ASSETS`), so the
 * SPA `dist/` must exist before any worker boots a Director. `npm run build` builds every
 * workspace, so the rd-console app gets its freshly built `@gridfpv/*` deps. We run it once here.
 *
 * The Director binary itself is built on demand by the boot harness (`startDirector`).
 */
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const frontendRoot = fileURLToPath(new URL('.', import.meta.url)).replace(/\/e2e\/?$/, '');

export default function globalSetup(): void {
  const built = spawnSync('npm', ['run', 'build'], {
    cwd: resolve(frontendRoot),
    stdio: 'inherit'
  });
  if (built.status !== 0) {
    throw new Error('frontend build failed (npm run build) — cannot serve the RD console SPA');
  }
}
