import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { svelteTesting } from '@testing-library/svelte/vite';

// The console is typed against `@gridfpv/types`, whose source re-exports the ts-rs
// bindings through the `@bindings/*` alias (see tsconfig.base.json). Vitest uses
// Vite's resolver, not tsconfig `paths`, so mirror the alias here pointing at the
// repo-root `bindings/`. Those imports are all `export type`, so nothing of the
// bindings executes — this only satisfies resolution. (Mirrors the component lib's
// vitest config.)
export default defineConfig({
  // Skip the Svelte preprocessor inside the Vitest worker (it trips on a partial Vite
  // environment); these screens are plain Svelte 5 + plain CSS, and production builds
  // still preprocess via svelte.config.js. Same trade-off the component library makes.
  plugins: [svelte({ preprocess: [] }), svelteTesting()],
  resolve: {
    // The workspace packages publish from `dist` (built by `npm run build`), but the
    // tests run against source so they don't need a prior build. Point each package at
    // its `src` entry; the svelte plugin handles the `.svelte` files behind the
    // component barrel. `@bindings` mirrors the tsconfig path alias (type-only).
    alias: {
      '@bindings': fileURLToPath(new URL('../../../bindings', import.meta.url)),
      '@gridfpv/components': fileURLToPath(
        new URL('../../packages/components/src/index.ts', import.meta.url)
      ),
      '@gridfpv/components/tokens.css': fileURLToPath(
        new URL('../../packages/components/src/tokens.css', import.meta.url)
      ),
      '@gridfpv/protocol-client': fileURLToPath(
        new URL('../../packages/protocol-client/src/index.ts', import.meta.url)
      ),
      '@gridfpv/types': fileURLToPath(new URL('../../packages/types/src/index.ts', import.meta.url))
    }
  },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./vitest.setup.ts'],
    include: ['tests/**/*.test.ts']
  }
});
