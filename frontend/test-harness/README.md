# GridFPV observability harness

> Full-stack observability **first**. When a UI breaks, see the WHOLE stack's picture at once —
> browser console, page errors, the Director's server log, and what crossed the WebSocket — with
> nobody adding ad-hoc `console.log`s after the fact. This directory is that harness. (#13, v0.4)

It boots the **real** Director (the `gridfpv` binary — same protocol API + RD console SPA an
operator runs) and captures the full stack's output in one place, for both automated tests and
hands-on debugging.

## Pieces

| File                      | What it is                                                                                                                                                                                                               |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `director.ts`             | Reusable Director-boot harness: `startDirector(opts?) -> { baseUrl, token, readLogs(), stop() }`. Framework-agnostic — usable from **vitest** (Node, no browser — the upcoming contract suite) and **Playwright** alike. |
| `../e2e/observability.ts` | Playwright fixture: `import { test, expect } from './observability.js'` and every spec auto-captures browser console + page errors + WS frames + the Director's server log, and **dumps them all together on failure**.  |
| `../e2e/global-setup.ts`  | Playwright global setup: builds the SPA `dist/` the worker Director serves.                                                                                                                                              |
| `observe.ts`              | Manual entry point — boot a Director, stream its server log, print the console URL + token, drive a sim race by hand.                                                                                                    |

## `startDirector(opts?)`

Spawns the built binary on an ephemeral (or given) port with a pinned RD token + sim env, buffers
its whole stdout+stderr, waits until it serves `GET /health`, and returns a handle. Builds the
binary on demand (`cargo build -p gridfpv-app`) if missing.

```ts
import { startDirector } from './test-harness/director.js';

const d = await startDirector({ simLaps: 4, simLapMs: 250 }); // ephemeral port, fresh token
// d.baseUrl  → "http://127.0.0.1:54123"
// d.token    → the pinned RD control token (also printed in the log)
// d.readLogs() → everything the Director has printed so far
await d.stop();
```

Options (all optional): `port`, `token`, `assets` (built SPA `dist/` for `GRIDFPV_ASSETS`),
`simLaps`, `simLapMs`, `env`, `build` (default `true`), `readyTimeoutMs` (default `30_000`).

The **contract suite** (next, vitest) should reuse `startDirector` verbatim: it boots the same
real Director, no browser, and `readLogs()` is the same server-log seam to assert against / dump.

## The Playwright on-failure dump

A failing spec prints (and attaches to the trace/report) one block:

```
══════════════════════════════════════════════════════════════════════════════
  FULL-STACK OBSERVABILITY DUMP (test failed — showing the whole picture)
══════════════════════════════════════════════════════════════════════════════

── BROWSER CONSOLE (N) ─────────────────────────────
  [ERROR] ...
── PAGE ERRORS (N) ─────────────────────────────────
  [PAGEERROR] Error: ... Cannot mix BigInt and other types ...
── WS FRAMES (N) ───────────────────────────────────
  · [OPEN] ws://127.0.0.1:39405/stream?token=...
  → [SENT] {"scope":{"Event":{"event":"event"}},"from":0}
  ← [RECEIVED] {...change envelope...}
── DIRECTOR SERVER LOG (tail) ──────────────────────
  GridFPV Director 0.1.0 — serving ...
══════════════════════════════════════════════════════════════════════════════
```

A render-time crash like the BigInt-in-render bug surfaces as a `[PAGEERROR]` line here with no
extra logging — that is the point.

## Manual observe

```sh
cd frontend
npm run build        # build the SPA the Director serves (else it serves the API only)
npm run observe      # boot the Director, stream its log, print the console URL + token
```

Then open the printed `RD console` URL, sign in with that address + token, and drive a heat by
hand. Server log streams in the terminal; open browser devtools for the browser console — the two
halves of the stack, side by side. Ctrl-C to stop.

Env overrides: `GRIDFPV_PORT` (default `8123`), `GRIDFPV_RD_TOKEN`, `GRIDFPV_SIM_LAPS`,
`GRIDFPV_SIM_LAP_MS`.

## Running the e2e

Needs chromium's system libs on this host (see how the e2e runs):

```sh
cd frontend
export LD_LIBRARY_PATH=/tmp/pwlibs/root/usr/lib/x86_64-linux-gnu:/tmp/pwlibs/root/usr/lib/x86_64-linux-gnu/gbm
npm run e2e
```
