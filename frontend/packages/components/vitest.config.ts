import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { svelteTesting } from '@testing-library/svelte/vite';

export default defineConfig({
  // The components are plain Svelte 5 + plain CSS, so they need no preprocessor.
  // Skipping `vitePreprocess()` here avoids Vite's `preprocessCSS` running inside
  // the Vitest worker (which trips on a partial Vite environment); production
  // builds still go through svelte.config.js's `vitePreprocess()` via svelte-package.
  plugins: [svelte({ preprocess: [] }), svelteTesting()],
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./vitest.setup.ts'],
    include: ['tests/**/*.test.ts']
  }
});
