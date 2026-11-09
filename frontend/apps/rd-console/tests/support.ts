/**
 * Test support: build a `Session` whose outward seams are mocked, so screens render and call
 * the session without a real server.
 */
import { vi } from 'vitest';
import type { EventMeta } from '@gridfpv/types';
import { Session, type SessionApi } from '../src/lib/session.svelte.js';

/** The event the screen tests open by default — an ordinary created event. */
const PRACTICE: EventMeta = {
  id: 'practice-ab12',
  name: 'Practice',
  created_at: 0,
  persistent: true
};

/**
 * The seams a screen test can override — **one key per {@link SessionApi} entry**, suffixed
 * `Impl` (`listPilotsImpl` steers `listPilots`). Derived from the session's own API record rather
 * than restated, so a new endpoint is overridable here the moment it exists and the two lists
 * cannot drift. All optional; anything a test leaves out keeps the default below.
 */
export type SessionSeams = { [K in keyof SessionApi as `${K}Impl`]?: SessionApi[K] };

/**
 * The seams a test actually named, keyed the way {@link Session} wants them.
 *
 * The `Impl` suffix is what marks an option as a **seam** rather than one of the seed values
 * ({@link makeTestSession}'s `event`, `noEnter`), so the whole options object can be handed over
 * and only the seams are read. A key passed as `undefined` is dropped, so a test may forward an
 * optional override straight through without blanking the default below.
 */
function seamOverrides(opts: SessionSeams | undefined): Partial<SessionApi> {
  return Object.fromEntries(
    Object.entries(opts ?? {})
      .filter(([key, impl]) => key.endsWith('Impl') && impl !== undefined)
      .map(([key, impl]) => [key.slice(0, -'Impl'.length), impl])
  ) as Partial<SessionApi>;
}

export interface TestSession {
  session: Session;
}

export function makeTestSession(
  opts?: {
    event?: EventMeta;
    /** Skip opening an event — for the app-level hub/page tests that render with no event. */
    noEnter?: boolean;
  } & SessionSeams
): TestSession {
  const session = new Session({
    baseUrl: 'http://d.local',
    autoRestore: false,
    api: {
      // The pilot read defaults to an INERT SUCCESS (an empty list), not the real fetch-backed
      // one: `fetch` is stubbed to fail below, and a failed directory read renders a visible
      // error state — which would otherwise leak into every test that doesn't name this seam.
      // Every other seam keeps its real implementation, which the stubbed `fetch` makes inert,
      // and a test that needs one to answer names it.
      listPilots: async () => [],
      ...seamOverrides(opts)
    }
  });
  // Seed a token (so gated writes don't trigger the lazy prompt) and open the event, unless
  // the test wants the app-level (no-event) context.
  session.setToken('tok');
  if (!opts?.noEnter) session.selectEvent(opts?.event ?? PRACTICE);

  vi.stubGlobal(
    'fetch',
    vi.fn(async () => ({ ok: false, json: async () => ({}) }) as unknown as Response)
  );

  return { session };
}
