import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vitest/config';

// The client is typed against `@gridfpv/types`, whose source re-exports the
// ts-rs bindings through the `@bindings/*` alias (see tsconfig.base.json). Vitest
// uses Vite's resolver, not tsconfig `paths`, so mirror the alias here pointing at
// the repo-root `bindings/`. Everything imported at runtime is `export type`, so
// nothing of the bindings actually executes — this only satisfies resolution.
export default defineConfig({
  resolve: {
    alias: {
      '@bindings': fileURLToPath(new URL('../../../bindings', import.meta.url))
    }
  },
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts']
  }
});
