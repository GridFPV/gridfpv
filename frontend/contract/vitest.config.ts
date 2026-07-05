import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vitest/config';

/**
 * The client↔server **contract suite** (#13, v0.4).
 *
 * This is the durable guard for the bug class that hit us in v0.4: every one of those
 * five bugs lived at a *mocked* boundary, so a unit test that mocked the seam stayed green
 * while the real wire disagreed. The suite here mocks **nothing** — it boots the real built
 * `gridfpv` Director (via the shared `test-harness/director.ts`) and drives it with raw
 * `fetch`/`WebSocket` *and* the real `@gridfpv/protocol-client` `dist`, asserting the actual
 * wire behaviour at every seam.
 *
 * Node environment (no jsdom): Node 22 supplies the global `fetch` + `WebSocket` the client
 * uses by default, so the same code path the browser runs is exercised here with no polyfill.
 *
 * Like the protocol-client's own vitest config, it mirrors the `@bindings` alias so the
 * `@gridfpv/types` import chain resolves; only `export type`s are pulled in, so nothing of
 * the bindings actually executes — this only satisfies resolution.
 */
export default defineConfig({
  resolve: {
    alias: {
      '@bindings': fileURLToPath(new URL('../../bindings', import.meta.url))
    }
  },
  test: {
    environment: 'node',
    include: ['**/*.contract.ts'],
    // Booting a real Director per file (build-on-demand on a cold checkout) is slow; give
    // each test room and run files serially so several Directors don't fight for the build
    // lock / ports on a cold cache.
    testTimeout: 30_000,
    hookTimeout: 60_000,
    fileParallelism: false
  }
});
