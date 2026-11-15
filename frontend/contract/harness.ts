/**
 * Shared helpers for the client↔server contract suite (#13).
 *
 * Everything here drives the **real** Director over the real wire — no mocks. The one
 * deliberate bit of configuration is {@link emptyAssetsDir}: the Director serves the RD
 * console SPA as a `fallback_service`, so an unmatched route normally returns `index.html`
 * (HTTP 200) when the console is built. Pointing `GRIDFPV_ASSETS` at an empty dir makes the
 * fallback deterministic — a `404` — so no test can mistake the console shell for an API answer.
 */
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import {
  type Director,
  type StartDirectorOptions,
  startDirector
} from '../test-harness/director.ts';

/** The `/events/{id}` route root the per-event routes hang off. */
export function eventRoot(baseUrl: string, eventId: string): string {
  return `${baseUrl}/events/${encodeURIComponent(eventId)}`;
}

/** A fresh empty dir to point `GRIDFPV_ASSETS` at, so the SPA fallback is a deterministic 404. */
export function emptyAssetsDir(): string {
  return mkdtempSync(join(tmpdir(), 'gridfpv-no-assets-'));
}

/** Boot a Director with a deterministic (no-SPA) assets dir layered on. */
export function startContractDirector(opts: StartDirectorOptions = {}): Promise<Director> {
  return startDirector({
    ...opts,
    env: { GRIDFPV_ASSETS: emptyAssetsDir(), ...(opts.env ?? {}) }
  });
}
