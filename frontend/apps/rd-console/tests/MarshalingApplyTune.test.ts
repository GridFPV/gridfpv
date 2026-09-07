import { describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import type { EventMeta, HeatSummary, LiveRaceState, Timer, TimerSignal } from '@gridfpv/types';
import Marshaling from '../src/screens/Marshaling.svelte';
import { makeTestSession } from './support.js';
import { lapList, signalTrace } from './fixtures.js';

/**
 * Marshaling → **Apply a discovered tune to the timer** (#470), at the screen.
 *
 * The unit half (which node, and did it land) is pinned in `applyTune.test.ts`. What this file
 * proves is the wiring the RD actually touches:
 *
 *  • the write goes down the **existing calibration path** (`session.setCalibration` →
 *    `POST /timers/{id}/calibration`) carrying both re-detected levels for the right node;
 *  • "landed" is reported from the **readback**, not from the write's own answer — RotorHazard
 *    never echoes a level set, so `On timer` may only appear once the signal feed says so (#403);
 *  • a refusal is **visible on the panel**, never just a dead disabled button (#405);
 *  • and applying is **independent of Commit** — it sends no marshaling command.
 */

const EVENT: EventMeta = {
  id: 'e1',
  name: 'Test event',
  created_at: 0,
  persistent: true,
  timers: ['rh-1'],
  roster: [],
  classes: []
};

/** ALICE flew lineup index 1 → node 1 → "Node 2". */
const HEATS: HeatSummary[] = [
  {
    heat: 'heat-1',
    name: 'heat-1',
    lineup: ['BOB', 'ALICE'],
    round: undefined,
    class: undefined,
    frequencies: [],
    phase: 'Unofficial',
    is_current: true
  } as HeatSummary
];

/** The marshaled heat is finished, so nothing is at risk on the timer — the ordinary case. */
const LIVE: LiveRaceState = {
  current_heat: 'heat-1',
  phase: 'Unofficial',
  active_pilots: ['ALICE', 'BOB'],
  progress: [{ competitor: 'ALICE', laps_completed: 2 }],
  running_order: ['ALICE', 'BOB']
} as LiveRaceState;

function timer(over: Partial<Timer> = {}): Timer {
  return {
    id: 'rh-1',
    name: 'Gate timer',
    kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
    status: 'Connected',
    channel_capability: 'Flexible',
    node_count: 4,
    available_channels: [],
    manual_connect: false,
    calibration: [],
    disabled_nodes: [],
    ...over
  } as Timer;
}

/** A signal snapshot in which node `n` reports `enter`/`exit`. */
function signalWith(n: number, enter?: number, exit?: number): TimerSignal {
  return {
    timer: 'rh-1',
    streaming: true,
    lease_ms_remaining: 5000,
    period_micros: 200_000,
    sample_micros: [],
    nodes: [
      {
        node: n,
        seat: `node-${n}`,
        seen: true,
        crossing: false,
        crossed_recently: false,
        samples: [],
        enter_at: enter,
        exit_at: exit
      }
    ]
  } as unknown as TimerSignal;
}

function setup(over: { timers?: Timer[]; signals?: TimerSignal[]; role?: 'rd' | 'readonly' } = {}) {
  const setCalibrationImpl = vi.fn(async () => undefined);
  const signals = over.signals ?? [signalWith(1, 110, 90)];
  let i = 0;
  const timerSignalImpl = vi.fn(async () => signals[Math.min(i++, signals.length - 1)]);
  const stopTimerSignalImpl = vi.fn(async () => undefined);

  const { session, sendSpy } = makeTestSession({
    event: EVENT,
    live: LIVE,
    laps: lapList,
    signal: signalTrace,
    role: over.role,
    listHeatsImpl: vi.fn(async () => HEATS),
    listTimersImpl: vi.fn(async () => over.timers ?? [timer()]),
    setCalibrationImpl: setCalibrationImpl as never,
    timerSignalImpl: timerSignalImpl as never,
    stopTimerSignalImpl: stopTimerSignalImpl as never
  });
  return { session, sendSpy, setCalibrationImpl, timerSignalImpl, stopTimerSignalImpl };
}

const applyButton = () => screen.getByTestId('apply-tune');

/** Nudge the enter box so the panel holds a level the RD chose. */
async function tuneTo(enter: number, exit: number) {
  await fireEvent.input(screen.getByLabelText('Enter threshold'), { target: { value: enter } });
  await fireEvent.input(screen.getByLabelText('Exit threshold'), { target: { value: exit } });
}

/** Render, and wait for the timer poll + heats read to land so the gate can resolve. */
async function renderReady(ctx: ReturnType<typeof setup>) {
  render(Marshaling, { session: ctx.session });
  await waitFor(() => expect(ctx.session.primaryTimer).toBeDefined());
  await waitFor(() => expect(screen.queryByTestId('apply-tune')).not.toBeNull());
  return ctx;
}

describe('Marshaling — Apply to timer (#470)', () => {
  it('writes BOTH re-detected levels to the node the competitor flew', async () => {
    const ctx = await renderReady(setup());
    await tuneTo(120, 85);
    await waitFor(() => expect(applyButton()).toBeEnabled());
    await fireEvent.click(applyButton());

    // ALICE is lineup[1] → node 1. Both levels travel together: a re-detection produces a pair.
    await waitFor(() =>
      expect(ctx.setCalibrationImpl).toHaveBeenCalledWith(
        expect.anything(),
        'rh-1',
        { node: 1, enter_at: 120, exit_at: 85 },
        expect.anything()
      )
    );
  });

  it('reports "On timer" only after the READBACK sees the levels, not on the write’s answer', async () => {
    // The first poll still shows the old levels; the second shows the new ones. `On timer` may not
    // appear until then — that is the whole point of the readback (#403).
    const ctx = await renderReady(
      setup({ signals: [signalWith(1, 100, 80), signalWith(1, 120, 85)] })
    );
    await tuneTo(120, 85);
    await waitFor(() => expect(applyButton()).toBeEnabled());
    await fireEvent.click(applyButton());

    await waitFor(
      () => expect(screen.getByTestId('apply-tune-state')).toHaveTextContent('On timer'),
      { timeout: 5000 }
    );
    // …and it names the gate it wrote, by friendly name (CLAUDE.md), never a raw seat.
    expect(screen.getByTestId('apply-tune-state')).toHaveTextContent('Node 2');
    expect(ctx.timerSignalImpl.mock.calls.length).toBeGreaterThanOrEqual(2);
    // The tuning stream is released rather than left running on its lease.
    expect(ctx.stopTimerSignalImpl).toHaveBeenCalled();
  }, 20_000);

  it('says "Not taken" — naming what the timer holds — when the levels never come back', async () => {
    const ctx = await renderReady(setup({ signals: [signalWith(1, 100, 80)] }));
    await tuneTo(120, 85);
    await waitFor(() => expect(applyButton()).toBeEnabled());
    await fireEvent.click(applyButton());

    // Deliberately slow: the panel waits CONFIRM_TIMEOUT_MS before calling a write not-taken,
    // because a mismatch declared one poll early is a false alarm.
    await waitFor(
      () => expect(screen.getByTestId('apply-tune-state')).toHaveTextContent('Not taken'),
      { timeout: 10_000 }
    );
    expect(screen.getByTestId('apply-tune-state')).toHaveTextContent('enter 100');
    expect(ctx.stopTimerSignalImpl).toHaveBeenCalled();
  }, 20_000);

  it('applying sends NO marshaling command — it is independent of Commit', async () => {
    const ctx = await renderReady(setup());
    await tuneTo(120, 85);
    await waitFor(() => expect(applyButton()).toBeEnabled());
    await fireEvent.click(applyButton());
    await waitFor(() => expect(ctx.setCalibrationImpl).toHaveBeenCalled());
    // Correcting this heat's laps and calibrating the gate are two different jobs.
    expect(ctx.sendSpy).not.toHaveBeenCalled();
  });

  it('refuses a disconnected timer ON THE PANEL, not just as a dead button', async () => {
    const ctx = setup({ timers: [timer({ status: 'Disconnected' })] });
    render(Marshaling, { session: ctx.session });
    await waitFor(() => expect(screen.queryByTestId('apply-tune-blocked')).not.toBeNull());
    expect(applyButton()).toBeDisabled();
    expect(screen.getByTestId('apply-tune-blocked')).toHaveTextContent(/not connected/i);
    expect(ctx.setCalibrationImpl).not.toHaveBeenCalled();
  });

  it('refuses a Mock — there is no gate behind it', async () => {
    const ctx = setup({
      timers: [timer({ kind: { Mock: { laps: 3, lap_ms: 30_000 } }, status: 'Ready' })]
    });
    render(Marshaling, { session: ctx.session });
    await waitFor(() => expect(screen.queryByTestId('apply-tune-blocked')).not.toBeNull());
    expect(screen.getByTestId('apply-tune-blocked')).toHaveTextContent(/Mock/);
  });

  it('offers nothing at all to a read-only session (the whole panel is role-gated)', async () => {
    const ctx = setup({ role: 'readonly' });
    render(Marshaling, { session: ctx.session });
    await waitFor(() => expect(ctx.session.primaryTimer).toBeDefined());
    expect(screen.queryByTestId('apply-tune')).toBeNull();
  });
});
