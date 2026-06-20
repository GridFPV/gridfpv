import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { svelteTesting } from '@testing-library/svelte/vite';

// Components are typed against `@gridfpv/types`, whose source re-exports the
// ts-rs bindings through the `@bindings/*` alias (see tsconfig.base.json). Vitest
// uses Vite's resolver, not tsconfig `paths`, so mirror the alias here pointing at
// the repo-root `bindings/`. Those imports are all `export type`, so nothing of
// the bindings executes — this only satisfies resolution.
export default defineConfig({
  // The components are plain Svelte 5 + plain CSS, so they need no preprocessor.
  // Skipping `vitePreprocess()` here avoids Vite's `preprocessCSS` running inside
  // the Vitest worker (which trips on a partial Vite environment); production
  // builds still go through svelte.config.js's `vitePreprocess()` via svelte-package.
  plugins: [svelte({ preprocess: [] }), svelteTesting()],
  resolve: {
    alias: {
      '@bindings': fileURLToPath(new URL('../../../bindings', import.meta.url))
    }
  },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./vitest.setup.ts'],
    include: ['tests/**/*.test.ts']
  }
});
