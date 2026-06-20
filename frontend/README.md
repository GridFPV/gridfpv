# GridFPV frontend

> One codebase, profiled three ways. See `docs/clients.html` §1.

Everything that renders GridFPV is a web client of the one protocol: the RD console
(in the Tauri window), the racer/spectator PWA, and the OBS overlays. They differ only in
permissions, transport, and emphasis — not in how they talk to the Director. This monorepo is
that shared frontend: one set of generated types, one thin protocol client, one component
library, and per-surface app entry points.

## Tooling decisions

| Choice          | Decision                                                        | Why                                                                                                                                                                                                                                                |
| --------------- | --------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Framework       | **Svelte 5** (runes)                                            | Decided in `docs/clients.html` §2: compile-away runtime → tiny bundles for lean overlays/PWA, fine-grained reactivity that maps onto a realtime change-stream, low ceremony for a non-frontend maintainer.                                         |
| Meta-framework  | **No SvelteKit** — plain Vite + `@sveltejs/vite-plugin-svelte`  | Surfaces are client-rendered SPAs embedded in a Tauri webview / OBS browser source. They don't need SSR, filesystem routing, or a Node server adapter; plain Vite keeps the build lean and the output static. Revisit if a surface ever needs SSR. |
| Build           | **Vite 6** (apps) + **`@sveltejs/package`** (component library) | `svelte-package` emits the component library with real `.d.ts` types so apps type-check against it; Vite builds the app bundles.                                                                                                                   |
| Package manager | **npm workspaces**                                              | No extra tooling; matches the project's lean ethos.                                                                                                                                                                                                |
| Language        | **TypeScript** (strict)                                         | First-class with Svelte 5 and with the ts-rs generated types.                                                                                                                                                                                      |
| Lint / format   | **ESLint 9 (flat config) + Prettier**, with Svelte plugins      | The well-supported, widely-known default — lowers the barrier to outside help.                                                                                                                                                                     |

## Layout

```
frontend/
├── package.json            # npm workspaces root; `npm run build` builds everything
├── tsconfig.base.json      # shared compiler options
├── eslint.config.js        # flat config, TS + Svelte
├── packages/
│   ├── types/              # @gridfpv/types — re-exports the generated bindings/*.ts
│   ├── protocol-client/    # @gridfpv/protocol-client — thin transport+subscribe layer (STUB, filled by #49)
│   └── components/         # @gridfpv/components — shared Svelte 5 component library (svelte-package)
└── apps/
    └── rd-console/         # @gridfpv/rd-console — the RD console surface (minimal shell, filled by #51+)
```

Future surfaces (`apps/spectator-pwa`, `apps/overlays`) slot in beside `rd-console` as new
workspace entries; they reuse `@gridfpv/components`, `@gridfpv/protocol-client`, and
`@gridfpv/types` unchanged.

## Generated types — the contract is generated, never transcribed

The wire types live in the Rust server crate and are generated to TypeScript via **ts-rs**
into the repo-root `bindings/` directory (one file per type, per `docs/clients.html` §3 and
`architecture.html` §6). **The frontend never hand-writes a wire type.**

`@gridfpv/types` is the single seam the rest of the frontend imports from. It re-exports the
generated bindings so every app and package imports protocol types from one place:

```ts
import type { RaceSnapshot, PilotId } from '@gridfpv/types';
```

How regenerated bindings flow in:

1. The Rust side regenerates `bindings/*.ts` (e.g. `cargo test` with ts-rs, or `xtask`).
2. `packages/types/src/index.ts` re-exports from the generated barrel. While `bindings/` has
   no barrel of its own, `packages/types/src/generated.ts` is the adapter that points at it
   (via the `tsconfig` path alias `@bindings/*` → `../../../bindings/*`).
3. Nothing else changes: apps already import from `@gridfpv/types`, so a contract change in
   Rust surfaces as a TypeScript compile error in the frontend rather than silent drift.

**Standalone-build note.** When `bindings/` is absent (e.g. a frontend-only checkout, or CI
that hasn't run the Rust generation step), `@gridfpv/types` falls back to a small set of
placeholder types in `src/generated.ts` so the monorepo still builds and type-checks. Once
real bindings exist, that fallback is replaced by the re-export — see the comments in
`packages/types/src/generated.ts`.

## Commands

```bash
npm install            # from frontend/ — installs all workspaces
npm run build          # build components + rd-console (and any other workspace)
npm run check          # svelte-check / tsc across workspaces
npm run lint           # eslint + prettier --check
npm run format         # prettier --write
npm run dev:rd-console # vite dev server for the RD console
```
