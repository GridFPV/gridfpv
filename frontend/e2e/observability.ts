/**
 * Playwright observability fixture (#13, v0.4 observability harness).
 *
 * The team's debugging rule is *full-stack observability FIRST*: when a UI breaks, you should see
 * the WHOLE stack's picture at once — what the browser logged, what JS blew up in the page, what
 * the Director's server log said, and what crossed the WebSocket — without anyone having added
 * ad-hoc `console.log`s after the fact. This fixture operationalizes that.
 *
 * Importing `test` from here (instead of `@playwright/test`) gives every spec, for free:
 *   • `page.on('console')`     — every browser console message (log/info/warn/**error**),
 *   • `page.on('pageerror')`   — every uncaught exception / unhandled rejection in the page
 *     (this is the one that catches a render-time crash like the BigInt-in-render bug — it
 *     surfaces as a `PAGEERROR` line in the dump with no extra logging),
 *   • `page.on('websocket')`   — the protocol WS: open/close + a tail of sent/received frames,
 *   • the Director's **server log** — the boot harness buffers the binary's stdout+stderr.
 *
 * On a **failing** test it dumps all four streams, together, to the test output (and attaches the
 * same report to the Playwright trace/report). On success it stays quiet.
 *
 * The Director is a **worker-scoped** fixture: one real `gridfpv` binary per worker, serving the
 * built RD console SPA, reused across the specs in that worker and torn down at the end. `baseURL`
 * is overridden to point at it, so specs just `page.goto('/')`.
 */
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test as base, expect } from '@playwright/test';
import { type Director, startDirector } from '../test-harness/director.js';

const here = fileURLToPath(new URL('.', import.meta.url));
const frontendRoot = resolve(here, '..');
const dist = resolve(frontendRoot, 'apps', 'rd-console', 'dist');

/** Fast sim defaults for the e2e: holeshot + 4 laps @ 250ms, same as the standalone config. */
const SIM_LAPS = Number(process.env.GRIDFPV_SIM_LAPS ?? 4);
const SIM_LAP_MS = Number(process.env.GRIDFPV_SIM_LAP_MS ?? 250);

/** One captured browser console message. */
interface ConsoleLine {
  ts: number;
  type: string;
  text: string;
}
/** One captured page error (uncaught exception / unhandled rejection in the page). */
interface PageErrorLine {
  ts: number;
  text: string;
}
/** One captured WS frame or lifecycle event. */
interface WsLine {
  ts: number;
  kind: 'open' | 'close' | 'sent' | 'received';
  url: string;
  payload?: string;
}

/** Keep the WS frame tail bounded — a long sim race emits a lot of change envelopes. */
const WS_FRAME_CAP = 80;

function clip(s: string, max = 400): string {
  return s.length > max ? `${s.slice(0, max)}… (${s.length} chars)` : s;
}

/** The per-test capture buffers the fixtures expose. */
interface Observability {
  console: ConsoleLine[];
  pageErrors: PageErrorLine[];
  ws: WsLine[];
}

/**
 * The worker's Director **plus the event its specs drive** (#414).
 *
 * There is no built-in event any more, so the fixture creates one — named "Practice", because
 * that is exactly what an RD makes for a warm-up session — and hands the specs its generated id.
 * Specs address per-event routes through `eventRoot` instead of a magic `/events/practice`.
 */
export interface E2eDirector extends Director {
  /** The id of the event the worker's specs drive. */
  readonly event: string;
  /** `${baseUrl}/events/${event}` — the per-event route root. */
  readonly eventRoot: string;
}

/** The display name the worker's event is created with (the picker row specs click). */
export const E2E_EVENT_NAME = 'Practice';

interface WorkerFixtures {
  director: E2eDirector;
}

interface TestFixtures {
  observability: Observability;
}

export const test = base.extend<TestFixtures, WorkerFixtures>({
  // ── One real Director per worker: builds + boots the binary, serves the built SPA ──────────
  director: [
    // Playwright requires the object-destructuring pattern here to extract fixture deps;
    // this fixture has none, so the pattern is empty.
    // eslint-disable-next-line no-empty-pattern
    async ({}, use) => {
      // Boot with NO token configured (`token: false`): control is open (full-trust by
      // default, #72 Slice 1b), so the console drives the whole race with no token step —
      // the lazy prompt never fires. (A token-gated path can be covered by a focused spec
      // that passes an explicit `token`.)
      const base = await startDirector({
        token: false,
        assets: dist,
        simLaps: SIM_LAPS,
        simLapMs: SIM_LAP_MS
      });
      // Create the event the specs drive. Control is open on this Director, so no token is
      // needed. (`first-run.spec.ts` boots its own Director to cover the genuinely empty picker.)
      const res = await fetch(`${base.baseUrl}/events`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: E2E_EVENT_NAME })
      });
      if (!res.ok) {
        await base.stop();
        throw new Error(`could not create the e2e event: HTTP ${res.status}`);
      }
      const { id } = (await res.json()) as { id: string };
      const director: E2eDirector = Object.assign(base, {
        event: id,
        eventRoot: `${base.baseUrl}/events/${id}`
      });
      await use(director);
      await director.stop();
    },
    { scope: 'worker' }
  ],

  // Point every spec's navigation at the worker's Director.
  baseURL: async ({ director }, use) => {
    await use(director.baseUrl);
  },

  // ── Per-test full-stack capture + on-failure dump ──────────────────────────────────────────
  observability: [
    async ({ page, director }, use, testInfo) => {
      const obs: Observability = { console: [], pageErrors: [], ws: [] };

      page.on('console', (msg) => {
        obs.console.push({ ts: Date.now(), type: msg.type(), text: msg.text() });
      });
      page.on('pageerror', (err) => {
        obs.pageErrors.push({ ts: Date.now(), text: err.stack ?? String(err) });
      });
      // Tap the protocol WebSocket: lifecycle + a bounded tail of frames both directions.
      page.on('websocket', (wsConn) => {
        const url = wsConn.url();
        obs.ws.push({ ts: Date.now(), kind: 'open', url });
        wsConn.on('framesent', (f) => {
          if (obs.ws.length < WS_FRAME_CAP) {
            obs.ws.push({ ts: Date.now(), kind: 'sent', url, payload: clip(String(f.payload)) });
          }
        });
        wsConn.on('framereceived', (f) => {
          if (obs.ws.length < WS_FRAME_CAP) {
            obs.ws.push({
              ts: Date.now(),
              kind: 'received',
              url,
              payload: clip(String(f.payload))
            });
          }
        });
        wsConn.on('close', () => obs.ws.push({ ts: Date.now(), kind: 'close', url }));
      });

      await use(obs);

      // After the test body: if it failed, dump the whole stack's picture together.
      const failed = testInfo.status !== testInfo.expectedStatus;
      if (failed) {
        const dump = renderDump(obs, director.readLogs());
        // To the console (visible in `list`/`line` reporters) …
        console.error(dump);
        // … and attached to the trace/HTML report for after-the-fact inspection.
        await testInfo.attach('full-stack-observability', {
          body: dump,
          contentType: 'text/plain'
        });
      }
    },
    { auto: true }
  ]
});

/** Render the four captured streams into one ordered, readable failure dump. */
function renderDump(obs: Observability, serverLog: string): string {
  const lines: string[] = [];
  const rule = '═'.repeat(78);
  lines.push('');
  lines.push(rule);
  lines.push('  FULL-STACK OBSERVABILITY DUMP (test failed — showing the whole picture)');
  lines.push(rule);

  lines.push('');
  lines.push(`── BROWSER CONSOLE (${obs.console.length}) ────────────────────────────────────────`);
  if (obs.console.length === 0) lines.push('  (none)');
  for (const c of obs.console) {
    lines.push(`  [${c.type.toUpperCase()}] ${clip(c.text, 500)}`);
  }

  lines.push('');
  lines.push(
    `── PAGE ERRORS (${obs.pageErrors.length}) ──────────────────────────────────────────`
  );
  if (obs.pageErrors.length === 0) lines.push('  (none)');
  for (const e of obs.pageErrors) {
    lines.push(`  [PAGEERROR] ${clip(e.text, 1500)}`);
  }

  lines.push('');
  lines.push(
    `── WS FRAMES (${obs.ws.length}${obs.ws.length >= WS_FRAME_CAP ? '+, capped' : ''}) ──────────────────────────────────`
  );
  if (obs.ws.length === 0) lines.push('  (none)');
  for (const w of obs.ws) {
    const arrow = w.kind === 'sent' ? '→' : w.kind === 'received' ? '←' : '·';
    lines.push(`  ${arrow} [${w.kind.toUpperCase()}] ${w.payload ?? w.url}`);
  }

  lines.push('');
  lines.push('── DIRECTOR SERVER LOG (tail) ────────────────────────────────────────────────');
  const tail = serverLog.split('\n').slice(-40).join('\n');
  lines.push(tail.length ? tail : '  (empty)');

  lines.push(rule);
  lines.push('');
  return lines.join('\n');
}

export { expect };
