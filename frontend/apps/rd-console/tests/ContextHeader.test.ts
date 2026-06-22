import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import type { LiveRaceState } from '@gridfpv/types';
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
