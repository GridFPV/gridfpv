import { describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import type {
  AuditEntry,
  EventMeta,
  HeatSummary,
  LapList,
  LiveRaceState,
  RoundDef
} from '@gridfpv/types';
import Marshaling from '../src/screens/Marshaling.svelte';
import { makeTestSession } from './support.js';
import {
  liveRunning,
  lapList,
  marshalingAudit,
  signalTrace,
  emptySignalTrace
} from './fixtures.js';

describe('Marshaling (Slice 3)', () => {
  it('renders the per-competitor selectable lap list', () => {
    const { session } = makeTestSession({ live: liveRunning, laps: lapList });
    render(Marshaling, { session });
    // Laps render as selectable buttons with number + duration.
    expect(screen.getByRole('button', { name: /Lap 1\s*41\.000/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Lap 2\s*40\.500/ })).toBeInTheDocument();
  });

  it('voids the SELECTED lap by its global end_ref (correct command target)', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList });
    render(Marshaling, { session });

    // Select ALICE's lap 2 (end_ref 14) and void it — the target must be 14, NOT a window offset.
    await fireEvent.click(screen.getByRole('button', { name: /Lap 2\s*40\.500/ }));
    await fireEvent.click(screen.getByRole('button', { name: 'Remove (void)' }));
    expect(sendSpy).toHaveBeenCalledWith({ VoidDetection: { target: 14 } });
  });

  it('splits the selected lap at the entered time, targeting its end_ref', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList });
    render(Marshaling, { session });

    // Marshal one pilot at a time: show BOB, then select his only lap (end_ref 13).
    await fireEvent.change(screen.getByLabelText('Pilot to marshal'), { target: { value: 'BOB' } });
    await fireEvent.click(screen.getByRole('button', { name: /Lap 1\s*43\.000/ }));
    await fireEvent.input(screen.getByLabelText('Correction time'), { target: { value: '21' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Split' }));
    expect(sendSpy).toHaveBeenCalledWith({ SplitLap: { target: 13, at: 21_000_000 } });
  });

  it('marshals a different heat WITHOUT moving Race Control’s current heat', async () => {
    const heats: HeatSummary[] = [
      {
        heat: 'heat-1',
        lineup: ['ALICE'],
        round: 'r1',
        class: 'c1',
        frequencies: [],
        phase: 'Final',
        is_current: true
      },
      {
        heat: 'heat-2',
        lineup: ['BOB'],
        round: 'r1',
        class: 'c1',
        frequencies: [],
        phase: 'Unofficial',
        is_current: false
      }
    ];
    const { session, sendSpy } = makeTestSession({
      live: liveRunning,
      laps: lapList,
      listHeatsImpl: vi.fn(async () => heats)
    });
    render(Marshaling, { session });
    // Pin a non-current heat to marshal it…
    await waitFor(() => expect(screen.getByLabelText('Heat to marshal')).toBeInTheDocument());
    await fireEvent.change(screen.getByLabelText('Heat to marshal'), {
      target: { value: 'heat-2' }
    });
    // …which must NOT issue a SetCurrentHeat — Race Control's current heat is untouched.
    const movedCurrent = sendSpy.mock.calls.find(
      ([c]) => typeof c === 'object' && c !== null && 'SetCurrentHeat' in c
    );
    expect(movedCurrent).toBeUndefined();
  });

  it('edits the selected lap time (AdjustLap on end_ref)', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList });
    render(Marshaling, { session });

    await fireEvent.click(screen.getByRole('button', { name: /Lap 1\s*41\.000/ }));
    await fireEvent.input(screen.getByLabelText('Correction time'), { target: { value: '40' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Edit time' }));
    // ALICE lap 1 end_ref = 12.
    expect(sendSpy).toHaveBeenCalledWith({ AdjustLap: { target: 12, at: 40_000_000 } });
  });

  it('applies a DQ to a competitor', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList });
    render(Marshaling, { session });

    await fireEvent.change(screen.getByLabelText('Ruling competitor'), {
      target: { value: 'BOB' }
    });
    // Kind defaults to Disqualify (no reason entered → the bare struct form).
    await fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    expect(sendSpy).toHaveBeenCalledWith({
      ApplyPenalty: { heat: 'heat-1', competitor: 'BOB', penalty: { Disqualify: {} } }
    });
  });

  it('reverses a prior ruling chosen from the audit', async () => {
    const { session, sendSpy } = makeTestSession({
      live: liveRunning,
      laps: lapList,
      audit: marshalingAudit
    });
    render(Marshaling, { session });

    // The reverse select offers the PenaltyApplied entry (at_ref 20).
    await fireEvent.change(screen.getByLabelText('Reverse ruling'), { target: { value: '20' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Reverse ruling' }));
    expect(sendSpy).toHaveBeenCalledWith({ ReverseRuling: { target: 20 } });
  });

  // ── Slice 6: full adjudication (DQ reason / points / throw-out / protests) ──────────────

  it('throws out the SELECTED lap (distinct from void) by its end_ref', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList });
    render(Marshaling, { session });
    // Select ALICE's lap 2 (end_ref 14) and throw it out — keeps the lap but drops it from scoring.
    await fireEvent.click(screen.getByRole('button', { name: /Lap 2\s*40\.500/ }));
    await fireEvent.click(screen.getByRole('button', { name: 'Throw out lap' }));
    expect(sendSpy).toHaveBeenCalledWith({ ThrowOutLap: { target: 14 } });
  });

  it('applies a DQ with a reason when one is entered', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList });
    render(Marshaling, { session });
    await fireEvent.change(screen.getByLabelText('Ruling competitor'), {
      target: { value: 'BOB' }
    });
    await fireEvent.input(screen.getByLabelText('DQ reason'), {
      target: { value: 'cut the course' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    expect(sendSpy).toHaveBeenCalledWith({
      ApplyPenalty: {
        heat: 'heat-1',
        competitor: 'BOB',
        penalty: { Disqualify: { reason: 'cut the course' } }
      }
    });
  });

  it('deducts standings points (not a per-heat effect)', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList });
    render(Marshaling, { session });
    await fireEvent.change(screen.getByLabelText('Ruling competitor'), {
      target: { value: 'BOB' }
    });
    await fireEvent.change(screen.getByLabelText('Penalty kind'), { target: { value: 'points' } });
    await fireEvent.input(screen.getByLabelText('Points to deduct'), { target: { value: '4' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    expect(sendSpy).toHaveBeenCalledWith({
      DeductPoints: { heat: 'heat-1', competitor: 'BOB', points: 4 }
    });
  });

  it('files a protest against a competitor with a note', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList });
    render(Marshaling, { session });
    await fireEvent.change(screen.getByLabelText('Protest competitor'), {
      target: { value: 'BOB' }
    });
    await fireEvent.input(screen.getByLabelText('Protest note'), {
      target: { value: 'contact on lap 2' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'File protest' }));
    expect(sendSpy).toHaveBeenCalledWith({
      FileProtest: { heat: 'heat-1', competitor: 'BOB', note: 'contact on lap 2' }
    });
  });

  it('resolves a filed protest from the audit with an outcome', async () => {
    const audit = [
      {
        kind: 'ProtestFiled' as const,
        at: 1_700_000_000_000_000,
        at_ref: 22,
        competitor: 'BOB',
        summary: 'Protest filed: contact'
      }
    ];
    const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList, audit });
    render(Marshaling, { session });
    await fireEvent.change(screen.getByLabelText('Resolve protest'), { target: { value: '22' } });
    await fireEvent.change(screen.getByLabelText('Protest outcome'), {
      target: { value: 'Denied' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Resolve protest' }));
    expect(sendSpy).toHaveBeenCalledWith({ ResolveProtest: { target: 22, outcome: 'Denied' } });
  });

  it('the reverse-ruling select offers throw-outs and heat-voids too (generalized reversal)', async () => {
    const audit = [
      {
        kind: 'LapThrownOut' as const,
        at: 1_700_000_000_000_000,
        at_ref: 30,
        competitor: null,
        summary: 'Lap thrown out (ref 14)'
      },
      {
        kind: 'HeatVoided' as const,
        at: 1_700_000_000_000_000,
        at_ref: 31,
        competitor: null,
        summary: 'Heat voided'
      }
    ];
    const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList, audit });
    render(Marshaling, { session });
    // The throw-out is reversible — pick it and reverse.
    await fireEvent.change(screen.getByLabelText('Reverse ruling'), { target: { value: '30' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Reverse ruling' }));
    expect(sendSpy).toHaveBeenCalledWith({ ReverseRuling: { target: 30 } });
  });

  it('void heat confirms first, then emits VoidHeat', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList });
    render(Marshaling, { session });

    await fireEvent.click(screen.getByRole('button', { name: 'Void heat' }));
    expect(sendSpy).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    expect(sendSpy).toHaveBeenCalledWith({ VoidHeat: { heat: 'heat-1' } });
  });

  it('finalizes the marshaled heat (Unofficial→Final), never moving the current heat', async () => {
    const live: LiveRaceState = {
      ...liveRunning,
      phase: 'Unofficial',
      lifecycle: { Provisional: {} }
    };
    const { session, sendSpy } = makeTestSession({ live, laps: lapList });
    render(Marshaling, { session });
    await fireEvent.click(screen.getByRole('button', { name: /Finalize/ }));
    expect(sendSpy).toHaveBeenCalledWith({ Finalize: { heat: 'heat-1' } });
    // Marshaling must not move Race Control's current heat.
    expect(
      sendSpy.mock.calls.find(([c]) => typeof c === 'object' && c !== null && 'SetCurrentHeat' in c)
    ).toBeUndefined();
  });

  it('blocks Finalize while a protest is open (P1-4)', async () => {
    const live: LiveRaceState = {
      ...liveRunning,
      phase: 'Unofficial',
      lifecycle: { Provisional: {} }
    };
    // One filed protest, no resolution → one open protest.
    const audit: AuditEntry[] = [
      {
        kind: 'ProtestFiled',
        at: 1_700_000_000_000_000,
        at_ref: 22,
        competitor: 'BOB',
        summary: 'Protest filed: contact'
      }
    ];
    const { session, sendSpy } = makeTestSession({ live, laps: lapList, audit });
    render(Marshaling, { session });

    const finalize = screen.getByRole('button', { name: /Finalize/ }) as HTMLButtonElement;
    expect(finalize.disabled).toBe(true);
    expect(screen.getByText(/Resolve 1 open protest/)).toBeInTheDocument();
    // Clicking the disabled gate sends no Finalize command.
    await fireEvent.click(finalize);
    expect(
      sendSpy.mock.calls.find(([c]) => typeof c === 'object' && c !== null && 'Finalize' in c)
    ).toBeUndefined();
  });

  it('re-enables Finalize once every open protest is resolved (P1-4)', async () => {
    const live: LiveRaceState = {
      ...liveRunning,
      phase: 'Unofficial',
      lifecycle: { Provisional: {} }
    };
    // A filed protest AND its resolution → no open protests.
    const audit: AuditEntry[] = [
      {
        kind: 'ProtestFiled',
        at: 1_700_000_000_000_000,
        at_ref: 22,
        competitor: 'BOB',
        summary: 'Protest filed: contact'
      },
      {
        kind: 'ProtestResolved',
        at: 1_700_000_000_000_001,
        at_ref: 23,
        competitor: 'BOB',
        summary: 'Protest resolved: denied'
      }
    ];
    const { session, sendSpy } = makeTestSession({ live, laps: lapList, audit });
    render(Marshaling, { session });

    const finalize = screen.getByRole('button', { name: /Finalize/ }) as HTMLButtonElement;
    expect(finalize.disabled).toBe(false);
    await fireEvent.click(finalize);
    expect(sendSpy).toHaveBeenCalledWith({ Finalize: { heat: 'heat-1' } });
  });

  it('reverts a finalized marshaled heat (Final→Unofficial) after confirm', async () => {
    const live: LiveRaceState = { ...liveRunning, phase: 'Final', lifecycle: 'Official' };
    const { session, sendSpy } = makeTestSession({ live, laps: lapList });
    render(Marshaling, { session });
    await fireEvent.click(screen.getByRole('button', { name: /Revert/ }));
    expect(sendSpy).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    expect(sendSpy).toHaveBeenCalledWith({ Revert: { heat: 'heat-1' } });
  });

  it('renders the audit trail newest-first', () => {
    const { session } = makeTestSession({
      live: liveRunning,
      laps: lapList,
      audit: marshalingAudit
    });
    render(Marshaling, { session });
    const panel = within(screen.getByRole('complementary', { name: 'Audit trail' }));
    const entries = panel.getAllByRole('listitem');
    // Newest first: the DQ (at_ref 20) precedes the void (at_ref 18). The competitor name is composed
    // from the STRUCTURED ref (resolved to its callsign — here the bare ref, no directory seeded).
    expect(entries[0]).toHaveTextContent('CARMEN · DQ applied');
    expect(entries[1]).toHaveTextContent('Detection voided (ref 12)');
  });

  // ── Slice 4: the signal-as-evidence RSSI graph ────────────────────────────────────────
  describe('signal-as-evidence graph (Slice 4)', () => {
    it('renders the graph with threshold lines + a lap marker per lap when a trace is present', () => {
      const { session } = makeTestSession({
        live: liveRunning,
        laps: lapList,
        signal: signalTrace
      });
      render(Marshaling, { session });

      // The graph mounts (one figure for ALICE's trace) with its enter/exit threshold lines.
      const graph = screen.getByLabelText('RSSI signal graph');
      const svg = within(graph).getByLabelText(/RSSI trace for ALICE/);
      expect(svg.querySelector('.enter-line')).not.toBeNull();
      expect(svg.querySelector('.exit-line')).not.toBeNull();

      // One vertical lap marker per ALICE lap (the lap list has two), each clickable.
      const markers = within(graph).getAllByRole('button', { name: /Lap \d+ at .* — select/ });
      expect(markers).toHaveLength(2);

      // The streaming-cadence note is surfaced so the coarse line isn't read as RH's dense history.
      expect(within(graph).getByText(/streaming-cadence/i)).toBeInTheDocument();
    });

    it('clicking a lap marker selects that lap in the action surface (two-way with the list)', async () => {
      const { session } = makeTestSession({
        live: liveRunning,
        laps: lapList,
        signal: signalTrace
      });
      render(Marshaling, { session });

      const graph = screen.getByLabelText('RSSI signal graph');
      // Click ALICE's lap-2 marker; the selection legend reflects exactly that lap.
      await fireEvent.click(within(graph).getByRole('button', { name: /Lap 2 at .* — select/ }));
      expect(screen.getByText(/Selected: ALICE · Lap 2/)).toBeInTheDocument();
      // The marker is now pressed (the two-way highlight).
      expect(within(graph).getByRole('button', { name: /Lap 2 at .* — select/ })).toHaveAttribute(
        'aria-pressed',
        'true'
      );
    });

    it('a selection made on the lap LIST highlights the matching graph marker (two-way)', async () => {
      const { session } = makeTestSession({
        live: liveRunning,
        laps: lapList,
        signal: signalTrace
      });
      render(Marshaling, { session });

      // Select via the lap list, assert the graph marker reflects it.
      await fireEvent.click(screen.getByRole('button', { name: /Lap 1\s*41\.000/ }));
      const graph = screen.getByLabelText('RSSI signal graph');
      expect(within(graph).getByRole('button', { name: /Lap 1 at .* — select/ })).toHaveAttribute(
        'aria-pressed',
        'true'
      );
    });

    it('clicking the trace adds a NEW lap at the cursor source-time (InsertLap)', async () => {
      const { session, sendSpy } = makeTestSession({
        live: liveRunning,
        laps: lapList,
        signal: signalTrace
      });
      render(Marshaling, { session });

      const graph = screen.getByLabelText('RSSI signal graph');
      const svg = within(graph).getByLabelText(/RSSI trace for ALICE/);
      // Pin the SVG box so clientX maps 1:1 onto the 0..1000 viewBox X (jsdom gives a 0-size rect).
      vi.spyOn(svg, 'getBoundingClientRect').mockReturnValue({
        left: 0,
        top: 0,
        right: 1000,
        bottom: 220,
        width: 1000,
        height: 220,
        x: 0,
        y: 0,
        toJSON: () => ({})
      } as DOMRect);

      // ALICE's trace spans 0..90s over plotW=984 from PAD_L=8 → click at X=500 ≈ 45.0s.
      await fireEvent.click(svg, { clientX: 500 });
      expect(sendSpy).toHaveBeenCalledTimes(1);
      const cmd = sendSpy.mock.calls[0][0] as { InsertLap: { competitor: string; at: number } };
      expect(cmd.InsertLap.competitor).toBe('ALICE');
      // X=500 → ((500-8)/984)*90s = 45.0s in source micros.
      expect(cmd.InsertLap.at).toBeGreaterThan(44_500_000);
      expect(cmd.InsertLap.at).toBeLessThan(45_500_000);
    });

    it('a heat with NO trace (a sim heat) skips the graph and keeps the lap-only layout', () => {
      const { session } = makeTestSession({
        live: liveRunning,
        laps: lapList,
        signal: emptySignalTrace
      });
      render(Marshaling, { session });

      // No graph; the lap list still renders (the sim fallback path).
      expect(screen.queryByLabelText('RSSI signal graph')).toBeNull();
      expect(screen.getByRole('button', { name: /Lap 1\s*41\.000/ })).toBeInTheDocument();
    });
  });

  // ── Add a brand-new lap (the explicit per-competitor control) ───────────────────────────────
  describe('add a new lap (explicit control)', () => {
    it('adds a lap at a typed time via InsertLap', async () => {
      const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList });
      render(Marshaling, { session });

      await fireEvent.change(screen.getByLabelText('Add-lap competitor'), {
        target: { value: 'ALICE' }
      });
      await fireEvent.input(screen.getByLabelText('Add-lap time'), { target: { value: '12.5' } });
      await fireEvent.click(screen.getByRole('button', { name: 'Add lap' }));
      // The command carries the marshaled heat so the server routes the insertion into THAT
      // heat's scoring window even when a different heat is live.
      expect(sendSpy).toHaveBeenCalledWith({
        InsertLap: { adapter: 'rh-1', competitor: 'ALICE', at: 12_500_000, heat: 'heat-1' }
      });
    });

    it('works for a competitor with ZERO existing laps', async () => {
      // A lap list where CARMEN has no laps at all — the control still adds one.
      const zeroLaps: LapList = {
        competitors: [
          { competitor: { adapter: 'rh-1', competitor: 'CARMEN' }, laps: [] },
          {
            competitor: { adapter: 'rh-1', competitor: 'ALICE' },
            laps: [
              { number: 1, duration_micros: 41_000_000, at: 41_000_000, start_ref: 10, end_ref: 12 }
            ]
          }
        ]
      };
      const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: zeroLaps });
      render(Marshaling, { session });

      // The empty competitor still renders + is selectable in the add-lap dropdown.
      expect(screen.getByText('No laps yet.')).toBeInTheDocument();
      await fireEvent.change(screen.getByLabelText('Add-lap competitor'), {
        target: { value: 'CARMEN' }
      });
      await fireEvent.input(screen.getByLabelText('Add-lap time'), { target: { value: '8' } });
      await fireEvent.click(screen.getByRole('button', { name: 'Add lap' }));
      expect(sendSpy).toHaveBeenCalledWith({
        InsertLap: { adapter: 'rh-1', competitor: 'CARMEN', at: 8_000_000, heat: 'heat-1' }
      });
    });

    it('is hidden for a read-only session', () => {
      const { session } = makeTestSession({
        live: liveRunning,
        laps: lapList,
        role: 'readonly'
      });
      render(Marshaling, { session });
      expect(screen.queryByLabelText('Add-lap competitor')).toBeNull();
      expect(screen.queryByRole('button', { name: 'Add lap' })).toBeNull();
    });
  });

  // ── Friendly names everywhere (heat name, lap headings, dropdowns, audit) ───────────────────
  //
  // The Marshaling raw-id bug: the screen rendered raw refs (pilot ids / "node-2") for the heat name,
  // the lap-list headings, the ruling/protest dropdowns, and the audit lines. These assert the screen
  // resolves them all to friendly names through the shared resolver + heat-name helper.
  describe('friendly names (the raw-id bug fix)', () => {
    // A roster-seeded heat: the competitor refs ARE the pilot ids (the common FromRoster case), so a
    // callsign must resolve from the directory with NO progress binding. node-2 is an unbound seat.
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
    const PILOTS = [
      { id: 'maverick-4d9rp8', callsign: 'Maverick', vtx_types: [] },
      { id: 'goose-yla6dp', callsign: 'Goose', vtx_types: [] }
    ];
    const FN_LIVE: LiveRaceState = {
      current_heat: 'q1-heat',
      phase: 'Unofficial',
      active_pilots: ['maverick-4d9rp8', 'goose-yla6dp'],
      progress: [
        { competitor: 'maverick-4d9rp8', laps_completed: 2, last_lap_micros: 40_000_000 },
        { competitor: 'goose-yla6dp', laps_completed: 2, last_lap_micros: 41_000_000 }
      ],
      running_order: ['maverick-4d9rp8', 'goose-yla6dp']
    };
    const FN_HEAT: HeatSummary = {
      heat: 'q1-heat',
      lineup: ['maverick-4d9rp8', 'goose-yla6dp'],
      round: 'r1',
      class: 'c1',
      frequencies: [],
      phase: 'Unofficial',
      is_current: true
    };
    // A lap list keyed by the pilot-id refs (so the headings must resolve to callsigns).
    const FN_LAPS: LapList = {
      competitors: [
        {
          competitor: { adapter: 'rh-1', competitor: 'maverick-4d9rp8' },
          laps: [
            { number: 1, duration_micros: 40_000_000, at: 40_000_000, start_ref: 10, end_ref: 12 }
          ]
        },
        {
          competitor: { adapter: 'rh-1', competitor: 'goose-yla6dp' },
          laps: [
            { number: 1, duration_micros: 41_000_000, at: 41_000_000, start_ref: 11, end_ref: 13 }
          ]
        }
      ]
    };
    // An audit whose competitor refs are pilot ids — the line must compose the resolved callsign.
    const FN_AUDIT: AuditEntry[] = [
      {
        kind: 'PenaltyApplied',
        at: 1_700_000_000_000_000,
        at_ref: 20,
        competitor: 'goose-yla6dp',
        summary: 'DQ applied'
      },
      {
        kind: 'ProtestFiled',
        at: 1_700_000_000_000_000,
        at_ref: 21,
        competitor: 'maverick-4d9rp8',
        summary: 'Protest filed: cut the course'
      }
    ];

    function renderFN(audit: AuditEntry[] = FN_AUDIT) {
      return makeTestSession({
        event: EVENT,
        live: FN_LIVE,
        laps: FN_LAPS,
        audit,
        listHeatsImpl: vi.fn(async () => [FN_HEAT]),
        listPilotsImpl: vi.fn(async () => PILOTS as unknown as never),
        listChannelsImpl: vi.fn(async () => [])
      });
    }

    it('renders the heat header as its friendly "<Round> Heat N" name, not the raw id', async () => {
      const { session } = renderFN();
      render(Marshaling, { session });
      const header = screen.getByRole('region', { name: 'Marshaling' }).querySelector('.heat')!;
      await waitFor(() => expect(header.textContent).toContain('Qualifying R1 Heat 1'));
      expect(header.textContent).not.toContain('q1-heat');
    });

    it('renders the lap-list headings as pilot callsigns, not the raw refs', async () => {
      const { session } = renderFN();
      render(Marshaling, { session });
      // One pilot at a time: each selected pilot's lap-list heading is their callsign, never the ref.
      await fireEvent.change(screen.getByLabelText('Pilot to marshal'), {
        target: { value: 'maverick-4d9rp8' }
      });
      await waitFor(() =>
        expect(screen.getByRole('heading', { name: 'Maverick' })).toBeInTheDocument()
      );
      await fireEvent.change(screen.getByLabelText('Pilot to marshal'), {
        target: { value: 'goose-yla6dp' }
      });
      expect(screen.getByRole('heading', { name: 'Goose' })).toBeInTheDocument();
      expect(screen.queryByText('maverick-4d9rp8')).not.toBeInTheDocument();
      expect(screen.queryByText('goose-yla6dp')).not.toBeInTheDocument();
    });

    it('labels the ruling + protest dropdowns by callsign (option value stays the ref)', async () => {
      const { session } = renderFN();
      render(Marshaling, { session });
      const ruling = screen.getByLabelText('Ruling competitor') as HTMLSelectElement;
      await waitFor(() => {
        const opts = Array.from(ruling.options).map((o) => o.textContent?.trim());
        expect(opts).toContain('Maverick');
        expect(opts).toContain('Goose');
      });
      // The option VALUE remains the raw ref (the command still targets it).
      const goose = Array.from(ruling.options).find((o) => o.textContent?.trim() === 'Goose')!;
      expect(goose.value).toBe('goose-yla6dp');
    });

    it('composes the audit line with the RESOLVED callsign from the structured ref', async () => {
      const { session } = renderFN();
      render(Marshaling, { session });
      const panel = within(screen.getByRole('complementary', { name: 'Audit trail' }));
      // The DQ line shows the callsign, never the raw pilot id.
      await waitFor(() => expect(panel.getByText('Goose · DQ applied')).toBeInTheDocument());
      expect(panel.getByText('Maverick · Protest filed: cut the course')).toBeInTheDocument();
      expect(panel.queryByText(/goose-yla6dp/)).not.toBeInTheDocument();
      expect(panel.queryByText(/maverick-4d9rp8/)).not.toBeInTheDocument();
    });

    // ── The actual regression #236 left open: the context-load race ─────────────────────────────
    //
    // #236 wired the resolvers but its tests always rendered with the event + its heats/pilots
    // available on the `$effect`s' FIRST run, so they never exercised a re-read. In the field the
    // Marshaling tab can mount **while the active event is still resolving** (a cold reload straight
    // onto it, or a remount before the live stream's first envelope): the heats/pilots reads then
    // land empty (`listHeats()` resolves `[]` with no event in hand yet). Keyed **only** off
    // `protocolState`, neither effect re-ran when `currentEvent` finally appeared — and a quiet
    // Unofficial heat emits no further stream tick — so `heats` (the friendly heat name) and
    // `pilots` (the callsigns) stayed empty and the header + lap headings showed raw ids.
    //
    // This test reproduces exactly that: the heats/pilots seams return EMPTY on their first call
    // (the resolving-window read) and the real data only on the next, and the test then nudges
    // `currentEvent` **without** touching `protocolState`. With the fix (the effects also depend on
    // `currentEvent`) the re-read fires and the names resolve; pre-fix they stay raw ids.
    // ── The node-seeded / finished-heat durable-binding fix (raw "node-0" bug) ──────────────────
    //
    // A node-seeded RH heat binds `node-0 → pilot` durably in the heat's `CompetitorRegistered`
    // facts (the heat-scope `?projection=live` fold carries it on `progress[].pilot`). But the
    // GLOBAL live stream (`session.liveState`) only has progress for the *current* heat — a
    // finished / non-current heat under review has empty global progress, and `node-0` is NOT a
    // directory pilot id, so the resolver fell through to the raw "node-0" channel/ref. The fix
    // sources `explicitPilotByRef` from the MARSHALED heat's own fold (`session.heatLiveState`),
    // so the callsign resolves for ANY heat with a durable binding — even with empty live progress.
    it('resolves the callsign for a node-seeded heat from the DURABLE binding (empty live progress)', async () => {
      // A node-seeded heat: the competitor ref is "node-0" (NOT a pilot id) and the heat is finished,
      // so the global live stream carries NO progress (current_heat is a different / no heat).
      const NODE_LAPS: LapList = {
        competitors: [
          {
            competitor: { adapter: 'rh-1', competitor: 'node-0' },
            laps: [
              { number: 1, duration_micros: 40_000_000, at: 40_000_000, start_ref: 10, end_ref: 12 }
            ]
          }
        ]
      };
      const NODE_HEAT: HeatSummary = {
        heat: 'q1-heat',
        lineup: ['node-0'],
        round: 'r1',
        class: 'c1',
        frequencies: [],
        phase: 'Unofficial',
        is_current: false
      };
      const NODE_AUDIT: AuditEntry[] = [
        {
          kind: 'PenaltyApplied',
          at: 1_700_000_000_000_000,
          at_ref: 20,
          competitor: 'node-0',
          summary: 'DQ applied'
        }
      ];
      // The GLOBAL stream is on this heat but carries NO pilot binding in progress (the regression
      // scenario): node-0 is unbound there. The DURABLE binding lives in the heat-scope fold.
      const EMPTY_LIVE: LiveRaceState = {
        current_heat: 'q1-heat',
        phase: 'Unofficial',
        active_pilots: ['node-0'],
        progress: [{ competitor: 'node-0', laps_completed: 1, last_lap_micros: 40_000_000 }],
        running_order: ['node-0']
      };
      // The marshaled heat's OWN fold (`?projection=live`) carries the durable `node-0 → Maverick` bind.
      const HEAT_LIVE: LiveRaceState = {
        current_heat: 'q1-heat',
        phase: 'Unofficial',
        active_pilots: ['node-0'],
        progress: [
          {
            competitor: 'node-0',
            pilot: 'maverick-4d9rp8',
            laps_completed: 1,
            last_lap_micros: 40_000_000
          }
        ],
        running_order: ['node-0']
      };
      const { session } = makeTestSession({
        event: EVENT,
        live: EMPTY_LIVE,
        heatLive: HEAT_LIVE,
        laps: NODE_LAPS,
        audit: NODE_AUDIT,
        listHeatsImpl: vi.fn(async () => [NODE_HEAT]),
        listPilotsImpl: vi.fn(async () => PILOTS as unknown as never),
        listChannelsImpl: vi.fn(async () => [])
      });
      render(Marshaling, { session });

      // Lap-list heading resolves to the callsign — not the raw "node-0".
      await waitFor(() =>
        expect(screen.getByRole('heading', { name: 'Maverick' })).toBeInTheDocument()
      );
      // The ruling / protest / add-lap dropdowns label by callsign (value stays the ref).
      const ruling = screen.getByLabelText('Ruling competitor') as HTMLSelectElement;
      const opts = Array.from(ruling.options).map((o) => o.textContent?.trim());
      expect(opts).toContain('Maverick');
      const mav = Array.from(ruling.options).find((o) => o.textContent?.trim() === 'Maverick')!;
      expect(mav.value).toBe('node-0');
      // The audit line composes the resolved callsign from the structured ref.
      const panel = within(screen.getByRole('complementary', { name: 'Audit trail' }));
      expect(panel.getByText('Maverick · DQ applied')).toBeInTheDocument();
      // The raw "node-0" must appear NOWHERE the resolver renders a name.
      expect(screen.queryByRole('heading', { name: 'node-0' })).not.toBeInTheDocument();
      expect(opts).not.toContain('node-0');
    });

    it('re-reads heats + pilots when currentEvent settles, with no further stream tick', async () => {
      let heatsCalls = 0;
      let pilotsCalls = 0;
      const { session } = makeTestSession({
        event: EVENT,
        live: FN_LIVE,
        laps: FN_LAPS,
        audit: FN_AUDIT,
        // First read (the resolving-window race) lands empty; the settled read returns the data.
        listHeatsImpl: vi.fn(async () => (heatsCalls++ === 0 ? [] : [FN_HEAT])),
        listPilotsImpl: vi.fn(async () => (pilotsCalls++ === 0 ? [] : PILOTS) as unknown as never),
        listChannelsImpl: vi.fn(async () => [])
      });
      render(Marshaling, { session });

      // After the first (empty) read the names fall back to raw ids — the bug's visible symptom.
      await waitFor(() => {
        const header = screen.getByRole('region', { name: 'Marshaling' }).querySelector('.heat');
        expect(header?.textContent).toContain('q1-heat');
      });

      // The active event settles — a fresh `EventMeta` assigned with no accompanying stream advance
      // (the heat is a quiet Unofficial, so `protocolState` does not change). This is the moment the
      // context must re-load.
      session.currentEvent = { ...EVENT };

      // The header heat name now resolves to its friendly "<Round> Heat N" and the lap-list headings
      // to callsigns — proving the heats/pilots context re-read on the `currentEvent` change alone.
      await waitFor(() => {
        const header = screen.getByRole('region', { name: 'Marshaling' }).querySelector('.heat');
        expect(header?.textContent).toContain('Qualifying R1 Heat 1');
      });
      const header = screen.getByRole('region', { name: 'Marshaling' }).querySelector('.heat')!;
      expect(header.textContent).not.toContain('q1-heat');
      // The shown pilot's lap-list heading resolves to a callsign (one pilot at a time via the picker).
      await fireEvent.change(screen.getByLabelText('Pilot to marshal'), {
        target: { value: 'maverick-4d9rp8' }
      });
      await waitFor(() =>
        expect(screen.getByRole('heading', { name: 'Maverick' })).toBeInTheDocument()
      );
      expect(screen.queryByText('maverick-4d9rp8')).not.toBeInTheDocument();
    });
  });

  it('a read-only session hides every mutating control but shows laps + audit', () => {
    const { session } = makeTestSession({
      live: liveRunning,
      laps: lapList,
      audit: marshalingAudit,
      role: 'readonly'
    });
    render(Marshaling, { session });

    // Laps and audit still render.
    expect(screen.getByRole('button', { name: /Lap 1\s*41\.000/ })).toBeInTheDocument();
    expect(screen.getByText('CARMEN · DQ applied')).toBeInTheDocument();
    // No mutating controls.
    expect(screen.queryByRole('button', { name: 'Remove (void)' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Split' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Apply' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Void heat' })).toBeNull();
    expect(screen.queryByLabelText('Reverse ruling')).toBeNull();
  });
});
