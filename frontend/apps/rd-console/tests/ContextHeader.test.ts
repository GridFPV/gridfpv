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
const liveAt = (phase: LiveRaceState['phase']): LiveRaceState =>
  ({ current_heat: 'heat-1', phase }) as LiveRaceState;

const clockEl = () => document.querySelector('.ctx-clock .gridfpv-race-clock');

describe('ContextHeader heat clock', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('ticks while Running, then stays frozen and visible through Unofficial and Final', async () => {
    const { session, pushLive } = makeTestSession({ live: liveAt('Running') });
    render(ContextHeader, { session, ongolive: () => {}, onswitchevent: () => {} });

    // The clock is on view while Running and advancing.
    await tick();
    expect(clockEl()).not.toBeNull();
    vi.advanceTimersByTime(4000);
    await tick();
    const atEnd = clockEl()?.textContent ?? '';
    expect(atEnd).not.toBe('0:00.000');

    // Time limit fires → Unofficial: the clock must remain visible AND stop changing.
    pushLive(liveAt('Unofficial'));
    await tick();
    expect(clockEl()).not.toBeNull();
    const frozen = clockEl()?.textContent ?? '';
    expect(frozen).toBe(atEnd);

    vi.advanceTimersByTime(5000);
    await tick();
    expect(clockEl()?.textContent).toBe(frozen);

    // Finalize → Final: still visible, still frozen.
    pushLive(liveAt('Final'));
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
