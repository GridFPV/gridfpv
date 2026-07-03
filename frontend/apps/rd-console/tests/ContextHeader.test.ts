import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import type { EventMeta, HeatSummary, LiveRaceState, RoundDef } from '@gridfpv/types';
import ContextHeader from '../src/ContextHeader.svelte';
import { makeTestSession } from './support.js';

/**
 * The persistent context-bar clock (#85) must mirror the live screen's heat clock: it ticks
 * while Running and then stays **visible, frozen** at the race-end time through Unofficial/Final,
 * rather than vanishing the instant the race closes (which made the header contradict the live
 * screen, where the frozen clock stays on view). Before the race it's hidden (the clock reads 0).
 */
// A `LiveRaceState` carrying the server's race timing (µs). The header clock is now
// server-time-authoritative (#62 follow-up): it counts from `race_started_at` and freezes at
// the exact `race_ended_at - race_started_at`.
const liveAt = (
  phase: LiveRaceState['phase'],
  opts: { startedAtMs?: number | null; endedAtMs?: number | null } = {}
): LiveRaceState =>
  ({
    current_heat: 'heat-1',
    phase,
    race_started_at: opts.startedAtMs == null ? undefined : opts.startedAtMs * 1000,
    race_ended_at: opts.endedAtMs == null ? undefined : opts.endedAtMs * 1000
  }) as LiveRaceState;

const clockEl = () => document.querySelector('.ctx-clock .gridfpv-race-clock');

describe('ContextHeader heat clock', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
  });
  afterEach(() => vi.useRealTimers());

  it('ticks while Running, then stays frozen and visible through Unofficial and Final', async () => {
    // Server race-go at t=0; the header anchors to it.
    const { session, pushLive } = makeTestSession({ live: liveAt('Running', { startedAtMs: 0 }) });
    render(ContextHeader, { session, ongolive: () => {}, onswitchevent: () => {} });

    // The clock is on view while Running and advancing.
    await tick();
    expect(clockEl()).not.toBeNull();
    vi.advanceTimersByTime(4000);
    await tick();
    const atEnd = clockEl()?.textContent ?? '';
    expect(atEnd).toBe('0:04.000');

    // Time limit fires → Unofficial at exactly 4.000s: the clock stays visible AND frozen at the
    // exact server duration.
    pushLive(liveAt('Unofficial', { startedAtMs: 0, endedAtMs: 4_000 }));
    await tick();
    expect(clockEl()).not.toBeNull();
    const frozen = clockEl()?.textContent ?? '';
    expect(frozen).toBe(atEnd);

    vi.advanceTimersByTime(5000);
    await tick();
    expect(clockEl()?.textContent).toBe(frozen);

    // Finalize → Final: still visible, still frozen at the exact value.
    pushLive(liveAt('Final', { startedAtMs: 0, endedAtMs: 4_000 }));
    await tick();
    vi.advanceTimersByTime(5000);
    await tick();
    expect(clockEl()).not.toBeNull();
    expect(clockEl()?.textContent).toBe(frozen);
  });

  it('hides the clock before the race (Scheduled) to avoid a misleading 0:00', async () => {
    const { session } = makeTestSession({ live: liveAt('Scheduled') });
    render(ContextHeader, { session, ongolive: () => {}, onswitchevent: () => {} });
    await tick();
    expect(clockEl()).toBeNull();
    // The heat + phase pill still render so the bar shows context.
    expect(screen.getByText('Scheduled')).not.toBeNull();
  });
});

/**
 * The persistent header showed the **raw heat id** in `.ctx-heat-id` on every screen. It must read
 * the friendly "‹Round› Heat N" name instead, resolved via `heatNameById` off the scheduled-heats
 * list + the event's rounds — the same name the Live-control title and the Marshaling header use.
 * The heats read mirrors Marshaling's #242 cold-load fix: it re-runs when `currentEvent` settles
 * (not only on a stream tick), so a header that mounts while the active event is still resolving
 * doesn't strand the raw id until the next stream advance.
 */
describe('ContextHeader heat name', () => {
  const ROUND = {
    id: 'r1',
    label: 'Qualifying R1',
    classes: ['c1'],
    format: 'timed_qual',
    params: {},
    win_condition: { Timed: { window_micros: 120_000_000 } },
    seeding: 'FromRoster',
    channel_mode: 'Static',
    protest_window: 'Off'
  } as unknown as RoundDef;
  const EVENT: EventMeta = {
    id: 'e1',
    name: 'Friday',
    created_at: 0,
    persistent: true,
    timers: ['mock'],
    roster: [],
    classes: ['c1'],
    rounds: [ROUND]
  };
  const HEAT: HeatSummary = {
    heat: 'q1-heat',
    lineup: [],
    round: 'r1',
    class: 'c1',
    frequencies: [],
    phase: 'Running',
    is_current: true
  };
  const live = (): LiveRaceState =>
    ({ current_heat: 'q1-heat', phase: 'Running' }) as LiveRaceState;

  const heatId = () => document.querySelector('.ctx-heat-id');

  it('shows the friendly "‹Round› Heat N" name, not the raw heat id, once heats+rounds resolve', async () => {
    const { session } = makeTestSession({
      event: EVENT,
      live: live(),
      listHeatsImpl: vi.fn(async () => [HEAT])
    });
    render(ContextHeader, { session, ongolive: () => {}, onswitchevent: () => {} });

    await waitFor(() => expect(heatId()?.textContent).toBe('Qualifying R1 Heat 1'));
    expect(heatId()?.textContent).not.toContain('q1-heat');
  });

  // The #242 cold-load race: the header mounts while the active event is still resolving, so the
  // first `listHeats()` lands EMPTY (no event in hand) and the name falls back to the raw id. When
  // `currentEvent` then settles with NO accompanying stream tick (a quiet heat), the read must
  // re-fire and the name must resolve — the effect depends on `currentEvent`, not just the stream.
  it('re-reads heats and resolves the name when currentEvent settles, with no stream tick', async () => {
    let calls = 0;
    const { session } = makeTestSession({
      event: EVENT,
      live: live(),
      listHeatsImpl: vi.fn(async () => (calls++ === 0 ? [] : [HEAT]))
    });
    render(ContextHeader, { session, ongolive: () => {}, onswitchevent: () => {} });

    // First (empty) read → raw id fallback, the visible symptom of the cold-load race.
    await waitFor(() => expect(heatId()?.textContent).toBe('q1-heat'));

    // The active event settles — a fresh EventMeta with no stream advance.
    session.currentEvent = { ...EVENT };

    // The re-read fires on the `currentEvent` change alone and the friendly name resolves.
    await waitFor(() => expect(heatId()?.textContent).toBe('Qualifying R1 Heat 1'));
    expect(heatId()?.textContent).not.toContain('q1-heat');
  });

  // #340: a FAILED heats read used to swallow into an empty list, so the header silently rendered
  // the raw heat id with no hint anything was wrong. It must show a visible error state (with an
  // explicit retry) in place of the heat name instead.
  it('shows an error state with retry — not the raw heat id — when the heats read fails (#340)', async () => {
    let fail = true;
    const { session } = makeTestSession({
      event: EVENT,
      live: live(),
      listHeatsImpl: vi.fn(async () => {
        if (fail) throw new Error('boom');
        return [HEAT];
      })
    });
    render(ContextHeader, { session, ongolive: () => {}, onswitchevent: () => {} });

    // The error state renders in place of the heat name — the raw id never reaches the screen.
    const retry = await screen.findByRole('button', { name: /Couldn.t load — retry/ });
    expect(heatId()).toBeNull();

    // Retry with the read healthy again: the friendly name resolves.
    fail = false;
    await fireEvent.click(retry);
    await waitFor(() => expect(heatId()?.textContent).toBe('Qualifying R1 Heat 1'));
    expect(screen.queryByRole('button', { name: /Couldn.t load — retry/ })).toBeNull();
  });
});
