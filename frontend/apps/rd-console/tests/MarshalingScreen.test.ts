import { describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor, within } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import type {
  AuditEntry,
  CommandAck,
  EventMeta,
  HeatSummary,
  LapList,
  LiveRaceState,
  RoundDef
} from '@gridfpv/types';
import { toasts } from '@gridfpv/components';
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

    // One click on the row's Remove: the target must be lap 2's end_ref 14, NOT a window offset.
    await fireEvent.click(screen.getByRole('button', { name: 'Remove lap 2' }));
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
    await fireEvent.click(screen.getByRole('button', { name: 'Throw out' }));
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
    // A filed protest AND its resolution (the server-baked summary carries the filing's offset,
    // which the open-count derivation matches against) → no open protests.
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
        competitor: null,
        summary: 'Protest denied (ref 22)'
      }
    ];
    const { session, sendSpy } = makeTestSession({ live, laps: lapList, audit });
    render(Marshaling, { session });

    const finalize = screen.getByRole('button', { name: /Finalize/ }) as HTMLButtonElement;
    expect(finalize.disabled).toBe(false);
    await fireEvent.click(finalize);
    expect(sendSpy).toHaveBeenCalledWith({ Finalize: { heat: 'heat-1' } });
  });

  it('counts a protest as OPEN again when its resolution was reversed (#340, matches the server gate)', async () => {
    const live: LiveRaceState = {
      ...liveRunning,
      phase: 'Unofficial',
      lifecycle: { Provisional: {} }
    };
    // Filed (#22) → resolved (#23, targeting #22) → the RESOLUTION reversed (#24, targeting #23).
    // The server's Finalize gate (`open_protest_count`, control_handler.rs) counts the protest as
    // open again; the old filed-minus-resolved UI count offered Finalize and the server rejected it.
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
        competitor: null,
        summary: 'Protest denied (ref 22)'
      },
      {
        kind: 'RulingReversed',
        at: 1_700_000_000_000_002,
        at_ref: 24,
        competitor: null,
        summary: 'Ruling reversed (ref 23)'
      }
    ];
    const { session, sendSpy } = makeTestSession({ live, laps: lapList, audit });
    render(Marshaling, { session });

    const finalize = screen.getByRole('button', { name: /Finalize/ }) as HTMLButtonElement;
    expect(finalize.disabled).toBe(true);
    expect(screen.getByText(/Resolve 1 open protest/)).toBeInTheDocument();
    await fireEvent.click(finalize);
    expect(
      sendSpy.mock.calls.find(([c]) => typeof c === 'object' && c !== null && 'Finalize' in c)
    ).toBeUndefined();
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

  // ── The official-result lock: a Final heat rejects result-changing corrections ───────────────
  //
  // The Director now bounces every result-changing marshaling command on a Final heat
  // (`require_not_final`, control_handler.rs) — the screen mirrors the gate: a prominent banner
  // carrying the WORKING Revert affordance, every correction surface withheld, protests exempt
  // (filing/resolving changes no result), and inspection (laps / audit / tune preview) still live.
  describe('official (Final) result lock', () => {
    const FINAL_LIVE: LiveRaceState = { ...liveRunning, phase: 'Final', lifecycle: 'Official' };

    it('shows the lock banner and withholds every correction surface on a Final heat', async () => {
      const { session, sendSpy } = makeTestSession({
        live: FINAL_LIVE,
        laps: lapList,
        audit: marshalingAudit,
        signal: signalTrace
      });
      render(Marshaling, { session });

      // The banner, with the Revert affordance IN it.
      const banner = screen.getByRole('status', { name: 'Official result lock' });
      expect(banner).toHaveTextContent(
        'This result is official — Revert it to make corrections. Protests may still be filed.'
      );
      expect(within(banner).getByRole('button', { name: /Revert/ })).toBeInTheDocument();

      // Laps + audit still render (inspection is fine)…
      expect(screen.getByRole('button', { name: /Lap 1\s*41\.000/ })).toBeInTheDocument();
      expect(screen.getByText('CARMEN · DQ applied')).toBeInTheDocument();

      // …but every result-changing surface is gone: the per-row Remove + the inline editor…
      expect(screen.queryByRole('button', { name: /Remove lap/ })).toBeNull();
      expect(screen.queryByRole('button', { name: 'Split' })).toBeNull();
      expect(screen.queryByRole('button', { name: 'Edit time' })).toBeNull();
      expect(screen.queryByRole('button', { name: 'Insert after' })).toBeNull();
      expect(screen.queryByRole('button', { name: 'Throw out' })).toBeNull();
      // …and the inline Add-lap control.
      expect(screen.queryByRole('button', { name: '+ Add lap' })).toBeNull();
      // …the penalty form + the reverse-ruling picker…
      expect(screen.queryByRole('button', { name: 'Apply' })).toBeNull();
      expect(screen.queryByLabelText('Reverse ruling')).toBeNull();
      // …and the heat-void danger zone.
      expect(screen.queryByRole('button', { name: 'Void heat' })).toBeNull();

      // The graph's add-lap path is unwired too: hovering the trace offers NO "Add lap" button
      // (the parent stops passing `onaddlap` down while the result is locked).
      const svg = screen.getByLabelText(/RSSI trace for ALICE/);
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
      await fireEvent.mouseMove(svg, { clientX: 500 });
      expect(screen.queryByRole('button', { name: /Add lap for ALICE/ })).toBeNull();

      // Nothing was sent by any of the above.
      expect(sendSpy).not.toHaveBeenCalled();
    });

    it('the banner Revert WORKS — it sends the command, and the Unofficial flip re-enables everything', async () => {
      const { session, sendSpy, pushLive } = makeTestSession({ live: FINAL_LIVE, laps: lapList });
      render(Marshaling, { session });

      // One click in the banner performs the Revert (same confirm semantics as the lifecycle
      // control — it reuses the same handler).
      const banner = screen.getByRole('status', { name: 'Official result lock' });
      await fireEvent.click(within(banner).getByRole('button', { name: /Revert/ }));
      expect(sendSpy).not.toHaveBeenCalled(); // confirm-first (destructive re-open)
      await fireEvent.click(within(banner).getByRole('button', { name: 'Confirm' }));
      expect(sendSpy).toHaveBeenCalledWith({ Revert: { heat: 'heat-1' } });

      // The phase flips to Unofficial (the stream re-folds): the lock lifts — banner gone,
      // correction surfaces back.
      pushLive({ ...liveRunning, phase: 'Unofficial', lifecycle: { Provisional: {} } });
      await waitFor(() =>
        expect(screen.getByRole('button', { name: 'Remove lap 1' })).toBeInTheDocument()
      );
      expect(screen.queryByRole('status', { name: 'Official result lock' })).toBeNull();
      expect(screen.getByRole('button', { name: '+ Add lap' })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: 'Void heat' })).toBeInTheDocument();
    });

    it('protests stay ENABLED on a Final heat — file and resolve both send (the exemption)', async () => {
      const audit: AuditEntry[] = [
        {
          kind: 'ProtestFiled',
          at: 1_700_000_000_000_000,
          at_ref: 22,
          competitor: 'BOB',
          summary: 'Protest filed: contact'
        }
      ];
      const { session, sendSpy } = makeTestSession({ live: FINAL_LIVE, laps: lapList, audit });
      render(Marshaling, { session });

      // Filing against the official result needs no Revert.
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

      // Resolving (e.g. denying) one needs no Revert either. (Wait for the file-protest submit to
      // settle first — the double-submit guard holds every correction button while one is in
      // flight, so the resolve click would otherwise be a legitimate no-op.)
      await fireEvent.change(screen.getByLabelText('Resolve protest'), { target: { value: '22' } });
      await fireEvent.change(screen.getByLabelText('Protest outcome'), {
        target: { value: 'Denied' }
      });
      await waitFor(() =>
        expect(screen.getByRole('button', { name: 'Resolve protest' })).toBeEnabled()
      );
      await fireEvent.click(screen.getByRole('button', { name: 'Resolve protest' }));
      await waitFor(() =>
        expect(sendSpy).toHaveBeenCalledWith({ ResolveProtest: { target: 22, outcome: 'Denied' } })
      );
    });
  });

  // ── Correction hardening: double-submit guard + amount validation (visible refusals) ─────────
  describe('correction hardening (double-submit + invalid amounts)', () => {
    it('a double-clicked Apply lands the penalty ONCE (the double-submit guard)', async () => {
      const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList });
      // A slow Director: the first send hangs until released — the window a double-click hits.
      let release!: (ack: CommandAck) => void;
      sendSpy.mockImplementationOnce(() => new Promise<CommandAck>((res) => (release = res)));
      render(Marshaling, { session });

      await fireEvent.change(screen.getByLabelText('Ruling competitor'), {
        target: { value: 'BOB' }
      });
      const apply = screen.getByRole('button', { name: 'Apply' });
      await fireEvent.click(apply);
      // While the first send is in flight the button disables AND the handler no-ops — the
      // second click of a double-click must not stack a second penalty.
      expect(apply).toBeDisabled();
      await fireEvent.click(apply);

      release({ ok: true });
      await waitFor(() => expect(apply).toBeEnabled());
      expect(sendSpy).toHaveBeenCalledTimes(1);
    });

    it('requires a positive time for Split / Edit time / Insert after (never sends at=0)', async () => {
      const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList });
      render(Marshaling, { session });

      // Select a lap with the time input still at its 0 default: the time-based corrections stay
      // DISABLED (with the explaining title) — an `AdjustLap at: 0` wrecks the whole lap chain.
      await fireEvent.click(screen.getByRole('button', { name: /Lap 1\s*41\.000/ }));
      for (const name of ['Split', 'Edit time', 'Insert after']) {
        const button = screen.getByRole('button', { name });
        expect(button).toBeDisabled();
        expect(button).toHaveAttribute('title', 'Enter a positive time (s) first');
      }
      await fireEvent.click(screen.getByRole('button', { name: 'Edit time' }));
      expect(sendSpy).not.toHaveBeenCalled();

      // An EMPTIED input (binds null) is refused the same way.
      await fireEvent.input(screen.getByLabelText('Correction time'), { target: { value: '' } });
      expect(screen.getByRole('button', { name: 'Edit time' })).toBeDisabled();

      // A positive time enables them and the command carries it.
      await fireEvent.input(screen.getByLabelText('Correction time'), { target: { value: '40' } });
      expect(screen.getByRole('button', { name: 'Edit time' })).toBeEnabled();
      await fireEvent.click(screen.getByRole('button', { name: 'Edit time' }));
      expect(sendSpy).toHaveBeenCalledWith({ AdjustLap: { target: 12, at: 40_000_000 } });
    });

    it('refuses an empty/zero penalty amount with a visible toast — never a silent no-op', async () => {
      const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList });
      toasts.clear();
      render(Marshaling, { session });
      await fireEvent.change(screen.getByLabelText('Ruling competitor'), {
        target: { value: 'BOB' }
      });

      // A time penalty with a CLEARED amount: refused + explained; nothing sent.
      await fireEvent.change(screen.getByLabelText('Penalty kind'), { target: { value: 'time' } });
      await fireEvent.input(screen.getByLabelText('Seconds'), { target: { value: '' } });
      await fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
      expect(sendSpy).not.toHaveBeenCalled();
      expect(toasts.items.some((t) => /positive number of seconds/.test(t.message))).toBe(true);

      // A ZERO points deduction: a 0-point DeductPoints is a no-op ruling — refused + explained.
      await fireEvent.change(screen.getByLabelText('Penalty kind'), {
        target: { value: 'points' }
      });
      await fireEvent.input(screen.getByLabelText('Points to deduct'), { target: { value: '0' } });
      await fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
      expect(sendSpy).not.toHaveBeenCalled();
      expect(toasts.items.some((t) => /whole number of points/.test(t.message))).toBe(true);

      // A valid amount still goes through.
      await fireEvent.input(screen.getByLabelText('Points to deduct'), { target: { value: '4' } });
      await fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
      await waitFor(() =>
        expect(sendSpy).toHaveBeenCalledWith({
          DeductPoints: { heat: 'heat-1', competitor: 'BOB', points: 4 }
        })
      );
    });
  });

  // ── The Recent-rulings strip (the full trail moved to the event-wide Audit page) ─────────────
  it('the Recent-rulings strip renders the latest entries newest-first (composed lines)', () => {
    const { session } = makeTestSession({
      live: liveRunning,
      laps: lapList,
      audit: marshalingAudit
    });
    render(Marshaling, { session });
    const panel = within(screen.getByRole('complementary', { name: 'Recent rulings' }));
    const entries = panel.getAllByRole('listitem');
    // Newest first: the DQ (at_ref 20) precedes the void (at_ref 18). The competitor name is composed
    // from the STRUCTURED ref (resolved to its callsign — here the bare ref, no directory seeded).
    expect(entries[0]).toHaveTextContent('CARMEN · DQ applied');
    // The void names no competitor structurally, but its target (ref 12) is ALICE's lap-1 end pass
    // in the lap list — the line resolves her name and renders the raw "(ref 12)" only as the
    // trailing "· #12" detail (#337 — the SHARED auditRender helpers).
    expect(entries[1]).toHaveTextContent('ALICE · Detection voided · #12');
    expect(entries[1]).not.toHaveTextContent('(ref 12)');
  });

  it('the strip shows at most the latest 3 entries — the full history lives on the Audit page', () => {
    // Four rulings, newest first (as the server serves them): only the first three render.
    const audit: AuditEntry[] = [40, 30, 20, 10].map((at_ref) => ({
      kind: 'PenaltyApplied' as const,
      at: 1_700_000_000_000_000 + at_ref,
      at_ref,
      competitor: 'BOB',
      summary: `DQ applied (strike ${at_ref})`
    }));
    const { session } = makeTestSession({ live: liveRunning, laps: lapList, audit });
    render(Marshaling, { session });
    const panel = within(screen.getByRole('complementary', { name: 'Recent rulings' }));
    expect(panel.getAllByRole('listitem')).toHaveLength(3);
    expect(panel.getByText(/strike 40/)).toBeInTheDocument();
    expect(panel.queryByText(/strike 10/)).toBeNull();
  });

  it('"View full audit →" jumps to the Audit tab pre-filtered to the MARSHALED heat', async () => {
    const onviewaudit = vi.fn();
    const { session } = makeTestSession({
      live: liveRunning,
      laps: lapList,
      audit: marshalingAudit
    });
    render(Marshaling, { session, onviewaudit });
    await fireEvent.click(screen.getByRole('button', { name: 'View full audit →' }));
    // The seam receives the marshaled heat as the prefilter (the shell deposits it + switches tab).
    expect(onviewaudit).toHaveBeenCalledWith({ heat: 'heat-1' });
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
      // The selection opens the lap's INLINE editor in the lap list (the action surface).
      expect(screen.getByLabelText('Edit lap 2')).toBeInTheDocument();
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

    it('a trace click sends NOTHING — only the explicit "Add lap here" button inserts', async () => {
      // No ruling may exist before an explicit action: the old click-to-insert path fired an
      // immediate InsertLap on ANY svg click — including the click the browser synthesizes when
      // a threshold drag ends inside the svg — planting phantom "Lap inserted" rulings
      // (live 2026-07-03).
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

      // A bare click on the trace (X=500 ≈ 45.0s) sends no command at all.
      await fireEvent.click(svg, { clientX: 500 });
      expect(sendSpy).not.toHaveBeenCalled();

      // The deliberate path still works: hover places the cursor, the labelled readout button
      // inserts — the exact heat-tagged InsertLap at the cursor's source-time.
      await fireEvent.mouseMove(svg, { clientX: 500 });
      await fireEvent.click(
        within(graph).getByRole('button', { name: /Add lap for ALICE at .* seconds/ })
      );
      expect(sendSpy).toHaveBeenCalledTimes(1);
      const cmd = sendSpy.mock.calls[0][0] as {
        InsertLap: { adapter: string; competitor: string; at: number; heat: string };
      };
      expect(cmd.InsertLap.adapter).toBe('rh-1');
      expect(cmd.InsertLap.competitor).toBe('ALICE');
      expect(cmd.InsertLap.heat).toBe('heat-1');
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

  // ── Voided passes: the shared removal record (void ↔ re-detection) ──────────────────────────
  describe('voided passes stay removed (the shared removal record)', () => {
    // ALICE's lap list with lap 2's closing pass VOIDED by the RD: one surviving lap plus the
    // removal record at 81.5s — the projection's `voided` field.
    const voidedLapList: LapList = {
      competitors: [
        {
          competitor: { adapter: 'rh-1', competitor: 'ALICE' },
          laps: [
            { number: 1, duration_micros: 41_000_000, at: 41_000_000, start_ref: 10, end_ref: 12 }
          ],
          voided: [{ at: 81_500_000, pass_ref: 14 }]
        }
      ]
    };

    it('renders the RD-voided pass struck-through in the lap list', () => {
      const { session } = makeTestSession({ live: liveRunning, laps: voidedLapList });
      render(Marshaling, { session });
      expect(screen.getByText(/removed pass at 1:21\.500s — stays removed/)).toBeInTheDocument();
    });

    it('re-detection SUPPRESSES a crossing the RD voided — never re-proposed as an add', async () => {
      // The trace still shows the 81.5s crossing (that's the whole bug): tuning must not
      // offer it back. Touch the levels to enter preview mode, then assert the crossing shows
      // as voided-stays-removed and the commit batch would not insert it.
      const { session, sendSpy } = makeTestSession({
        live: liveRunning,
        laps: voidedLapList,
        signal: signalTrace
      });
      render(Marshaling, { session });

      // Nudge the ENTER level (explicit intent) to values that detect BOTH peaks.
      await fireEvent.input(screen.getByLabelText('Enter threshold'), {
        target: { value: '111' }
      });
      // The summary counts the suppressed crossing separately — never as an add.
      const summary = await screen.findByTestId('redetect-summary');
      expect(summary.textContent).toContain('voided by you stay removed');
      expect(summary.textContent).toContain('+0 added');
      // The lap box (in preview mode — the re-detection also drops the phantom 0s opening
      // pass) marks the suppressed crossing at its DETECTED instant, clearly non-addable.
      expect(
        screen.getByText(/crossing at 1:21\.000s — voided by you, stays removed/)
      ).toBeInTheDocument();
      // Committing sends ONLY the legitimate removal — and, crucially, NO InsertLap for the
      // suppressed crossing (the whole bug: the tuner used to re-add the voided lap).
      await fireEvent.click(screen.getByRole('button', { name: 'Commit re-detection' }));
      await waitFor(() => expect(sendSpy).toHaveBeenCalled());
      const inserts = sendSpy.mock.calls.filter(
        ([c]) => typeof c === 'object' && c !== null && 'InsertLap' in c
      );
      expect(inserts).toEqual([]);
    });

    it('the lap box only switches to preview on EXPLICIT tuning intent', () => {
      // A trace whose recorded thresholds disagree with the official laps must not hijack the
      // interactive lap list on its own (no drag/edit yet → the normal rows + actions render).
      const { session } = makeTestSession({
        live: liveRunning,
        laps: voidedLapList,
        signal: signalTrace
      });
      render(Marshaling, { session });
      expect(screen.queryByText(/re-detection preview/)).toBeNull();
      expect(screen.getByRole('button', { name: 'Remove lap 1' })).toBeInTheDocument();
    });
  });

  // ── Add a brand-new lap (the explicit per-competitor control) ───────────────────────────────
  describe('add a new lap (explicit control)', () => {
    it('adds a lap at a typed time via InsertLap', async () => {
      const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList });
      render(Marshaling, { session });

      // ALICE is the shown pilot; her lap box carries the inline Add control.
      await fireEvent.click(screen.getByRole('button', { name: '+ Add lap' }));
      await fireEvent.input(screen.getByLabelText('Add-lap time'), { target: { value: '12.5' } });
      await fireEvent.click(screen.getByRole('button', { name: 'Add' }));
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

      // Show CARMEN (zero laps): her box renders empty WITH the inline Add control.
      await fireEvent.change(screen.getByLabelText('Pilot to marshal'), {
        target: { value: 'CARMEN' }
      });
      expect(screen.getByText('No laps yet.')).toBeInTheDocument();
      await fireEvent.click(screen.getByRole('button', { name: '+ Add lap' }));
      await fireEvent.input(screen.getByLabelText('Add-lap time'), { target: { value: '8' } });
      await fireEvent.click(screen.getByRole('button', { name: 'Add' }));
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
  describe('a competitor under two adapters (mid-heat failover / a re-raced heat)', () => {
    // The SAME ref can hold two lap-list entries — one per adapter (a mid-heat source
    // failover; historically also a re-raced heat before the current-run window fix). Bare-ref
    // {#each} keys collided on it and CRASHED the whole screen (svelte each_key_duplicate) —
    // the "can't slide anything anymore" report from live testing 2026-07-03.
    const DUP_LAPS: LapList = {
      competitors: [
        {
          competitor: { adapter: 'sim', competitor: 'ALICE' },
          laps: [
            { number: 1, duration_micros: 40_000_000, at: 41_000_000, start_ref: 2, end_ref: 4 }
          ]
        },
        {
          competitor: { adapter: 'rh-1', competitor: 'ALICE' },
          laps: [
            { number: 1, duration_micros: 39_000_000, at: 40_000_000, start_ref: 10, end_ref: 12 }
          ]
        }
      ]
    };

    it('renders without crashing and lists the pilot once in the pickers', () => {
      const { session } = makeTestSession({ live: liveRunning, laps: DUP_LAPS });
      render(Marshaling, { session });
      // Both adapter entries' lap sections render (keyed by the full adapter/ref key)…
      expect(screen.getAllByRole('button', { name: /Lap 1/ }).length).toBe(2);
      // …and the marshal-pilot picker lists ALICE exactly once (deduped refs).
      const picker = screen.getByLabelText('Marshal pilot') as HTMLSelectElement;
      const values = Array.from(picker.options).map((o) => o.value);
      expect(values.filter((v) => v === 'ALICE').length).toBe(1);
    });
  });

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

    it('composes the strip line with the RESOLVED callsign from the structured ref', async () => {
      const { session } = renderFN();
      render(Marshaling, { session });
      const panel = within(screen.getByRole('complementary', { name: 'Recent rulings' }));
      // The DQ line shows the callsign, never the raw pilot id.
      await waitFor(() => expect(panel.getByText('Goose · DQ applied')).toBeInTheDocument());
      expect(panel.getByText('Maverick · Protest filed: cut the course')).toBeInTheDocument();
      expect(panel.queryByText(/goose-yla6dp/)).not.toBeInTheDocument();
      expect(panel.queryByText(/maverick-4d9rp8/)).not.toBeInTheDocument();
    });

    // ── #337: the audit + "Reverse a ruling" picker leaked raw log offsets / unresolved refs ──────
    //
    // The server bakes "(ref N)" into some summaries and names no competitor structurally for the
    // lap-/target-addressed rulings. The screen must re-compose the line from the structured fields:
    // a lap-addressed target resolves through the lap list, a protest-resolution / reversal chases
    // its target audit entry — so the picker reads "‹what› — ‹callsign› · #‹offset›", never "(ref N)".
    it('labels the reverse-ruling picker "‹what› — ‹callsign›" with the offset only trailing (#337)', async () => {
      const audit: AuditEntry[] = [
        // Named structurally — resolves directly.
        {
          kind: 'PenaltyApplied',
          at: 1_700_000_000_000_000,
          at_ref: 20,
          competitor: 'goose-yla6dp',
          summary: 'DQ applied'
        },
        // Filed protest (the resolution below targets it).
        {
          kind: 'ProtestFiled',
          at: 1_700_000_000_000_001,
          at_ref: 21,
          competitor: 'maverick-4d9rp8',
          summary: 'Protest filed: contact'
        },
        // Lap-addressed: no structured competitor; ref 12 is Maverick's lap end pass in FN_LAPS.
        {
          kind: 'LapThrownOut',
          at: 1_700_000_000_000_002,
          at_ref: 30,
          competitor: null,
          summary: 'Lap thrown out (ref 12)'
        },
        // Target-addressed: resolves by chasing the filed entry at ref 21 (Maverick).
        {
          kind: 'ProtestResolved',
          at: 1_700_000_000_000_003,
          at_ref: 32,
          competitor: null,
          summary: 'Protest upheld (ref 21)'
        }
      ];
      const { session } = renderFN(audit);
      render(Marshaling, { session });

      const reverse = screen.getByLabelText('Reverse ruling') as HTMLSelectElement;
      await waitFor(() => {
        const labels = Array.from(reverse.options).map((o) => o.textContent?.trim());
        expect(labels).toContain('DQ applied — Goose · #20');
        expect(labels).toContain('Lap thrown out — Maverick · #30');
        expect(labels).toContain('Protest upheld — Maverick · #32');
      });
      // No option leaks a raw "(ref N)" or an unresolved pilot-id ref; values stay the offsets.
      const labels = Array.from(reverse.options).map((o) => o.textContent ?? '');
      expect(labels.some((l) => l.includes('(ref'))).toBe(false);
      expect(labels.some((l) => l.includes('goose-yla6dp') || l.includes('maverick-4d9rp8'))).toBe(
        false
      );
      const thrownOut = Array.from(reverse.options).find((o) =>
        o.textContent?.includes('Lap thrown out')
      )!;
      expect(thrownOut.value).toBe('30');

      // The resolve-protest picker composes the same way.
      const resolve = screen.getByLabelText('Resolve protest') as HTMLSelectElement;
      const resolveLabels = Array.from(resolve.options).map((o) => o.textContent?.trim());
      expect(resolveLabels).toContain('Protest filed: contact — Maverick · #21');

      // The strip's lines resolve the same targets (SHARED helpers): callsign first, the offset
      // only as "· #N", never a server-baked "(ref N)". The lap-addressed throw-out (3rd-newest,
      // within the latest-3 strip) resolves Maverick through the lap list.
      const panel = within(screen.getByRole('complementary', { name: 'Recent rulings' }));
      expect(panel.getByText('Maverick · Lap thrown out · #12')).toBeInTheDocument();
      expect(panel.queryByText(/\(ref \d+\)/)).not.toBeInTheDocument();
    });

    // ── #340: a failed pilots/heats read must surface, not silently strand raw refs ──────────────
    it('surfaces a visible retry state when the pilot/heat directory reads fail (#340)', async () => {
      let fail = true;
      const { session } = makeTestSession({
        event: EVENT,
        live: FN_LIVE,
        laps: FN_LAPS,
        audit: FN_AUDIT,
        listHeatsImpl: vi.fn(async () => {
          if (fail) throw new Error('boom');
          return [FN_HEAT];
        }),
        listPilotsImpl: vi.fn(async () => {
          if (fail) throw new Error('boom');
          return PILOTS as unknown as never;
        }),
        listChannelsImpl: vi.fn(async () => [])
      });
      render(Marshaling, { session });

      // The failure is visible — no more silently-empty directory + raw refs.
      const alert = await screen.findByRole('alert');
      expect(alert).toHaveTextContent(/Couldn.t load the pilot\/heat directory/);

      // Retry with the reads healthy again: the error clears and the names resolve.
      fail = false;
      await fireEvent.click(within(alert).getByRole('button', { name: 'Try again' }));
      await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
      await waitFor(() =>
        expect(screen.getByRole('heading', { name: 'Maverick' })).toBeInTheDocument()
      );
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
      // The strip line composes the resolved callsign from the structured ref.
      const panel = within(screen.getByRole('complementary', { name: 'Recent rulings' }));
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

  // ── Tune detection (the RH-style threshold re-detection with manual commit) ──────────────────
  describe('tune detection (threshold re-detection)', () => {
    // A tuning-friendly fixture: ALICE's trace has clean 1-sample peaks at exactly her official
    // passes (holeshot 1s, gates 41s + 81s) plus a SMALLER peak at 60s that only crosses when the
    // enter level is lowered. Laps are timed so `officialPasses` reconstructs [1s, 41s, 81s].
    const TUNE_LAPS: LapList = {
      competitors: [
        {
          competitor: { adapter: 'rh-1', competitor: 'ALICE' },
          laps: [
            { number: 1, duration_micros: 40_000_000, at: 41_000_000, start_ref: 10, end_ref: 12 },
            { number: 2, duration_micros: 40_000_000, at: 81_000_000, start_ref: 12, end_ref: 14 }
          ]
        }
      ]
    };
    const TUNE_TRACE = {
      competitors: [
        {
          competitor: { adapter: 'rh-1', competitor: 'ALICE' },
          from: 0,
          period_micros: 1_000_000,
          // Baseline 70; peaks: 130 @1s (holeshot), 132 @41s, 105 @60s (sub-threshold), 130 @81s.
          samples: Array.from({ length: 91 }, (_, i) =>
            i === 1 ? 130 : i === 41 ? 132 : i === 60 ? 105 : i === 81 ? 130 : 70
          ),
          enter: 110,
          exit: 95
        }
      ]
    };

    function renderTune(role?: 'readonly') {
      return makeTestSession({
        live: liveRunning,
        laps: TUNE_LAPS,
        signal: TUNE_TRACE,
        role
      });
    }

    it('shows the panel (seeded from the recorded thresholds) when a trace + control are present', () => {
      const { session } = renderTune();
      render(Marshaling, { session });
      expect(screen.getByText(/Tune detection/)).toBeInTheDocument();
      // Inputs initialize from the trace's recorded enter/exit.
      expect((screen.getByLabelText('Enter threshold') as HTMLInputElement).value).toBe('110');
      expect((screen.getByLabelText('Exit threshold') as HTMLInputElement).value).toBe('95');
      // At the recorded levels the re-detection matches the official passes: nothing to commit.
      expect(screen.getByTestId('redetect-summary')).toHaveTextContent(
        'Would be 2 laps (+0 added, −0 removed)'
      );
      expect(
        (screen.getByRole('button', { name: 'Commit re-detection' }) as HTMLButtonElement).disabled
      ).toBe(true);
    });

    it('is hidden without a trace and for a read-only session', () => {
      const noTrace = makeTestSession({
        live: liveRunning,
        laps: TUNE_LAPS,
        signal: emptySignalTrace
      });
      render(Marshaling, { session: noTrace.session });
      expect(screen.queryByText(/Tune detection/)).toBeNull();
      cleanup();
      const { session } = renderTune('readonly');
      render(Marshaling, { session });
      expect(screen.queryByText(/Tune detection/)).toBeNull();
    });

    it('adjusting a threshold updates the LIVE preview without sending anything', async () => {
      const { session, sendSpy } = renderTune();
      render(Marshaling, { session });

      // Lower enter to 100: the 105-peak at 60s now crosses → one ADDED pass, a 3-lap preview.
      await fireEvent.input(screen.getByLabelText('Enter threshold'), {
        target: { value: '100' }
      });
      expect(screen.getByTestId('redetect-summary')).toHaveTextContent(
        'Would be 3 laps (+1 added, −0 removed)'
      );
      // The UNIFIED preview list: one chronological story — the surviving laps with the new lap
      // accent-marked. The 41→60s split makes lap 2 the ADDED one (19s), laps 1 + 3 kept.
      const previewList = screen.getByLabelText(/Re-detection preview for ALICE/);
      const rows = previewList.querySelectorAll('li');
      expect(rows).toHaveLength(3);
      expect(rows[0].classList.contains('kept')).toBe(true);
      expect(rows[0]).toHaveTextContent('Lap 1');
      expect(rows[0]).toHaveTextContent('40.000');
      expect(rows[1].classList.contains('added')).toBe(true);
      expect(rows[1]).toHaveTextContent('+ Lap 2');
      expect(rows[1]).toHaveTextContent('19.000');
      expect(rows[2].classList.contains('kept')).toBe(true);
      expect(rows[2]).toHaveTextContent('Lap 3');
      expect(rows[2]).toHaveTextContent('21.000');
      // NOTHING was sent while adjusting — commit is explicit.
      expect(sendSpy).not.toHaveBeenCalled();
      expect(
        (screen.getByRole('button', { name: 'Commit re-detection' }) as HTMLButtonElement).disabled
      ).toBe(false);
    });

    it('passes the new levels DROP interleave as struck "removed" rows in the one preview list', async () => {
      const { session, sendSpy } = renderTune();
      render(Marshaling, { session });

      // Raise enter above every peak but the 132 one: only the 41s pass survives — the holeshot
      // (1s) and the 81s gate pass leave the record. They render as removed rows, in time order,
      // in the SAME list (no side-by-side preview-vs-official confusion).
      await fireEvent.input(screen.getByLabelText('Enter threshold'), {
        target: { value: '131' }
      });
      expect(screen.getByTestId('redetect-summary')).toHaveTextContent(
        'Would be 0 laps (+0 added, −2 removed)'
      );
      const previewList = screen.getByLabelText(/Re-detection preview for ALICE/);
      const rows = previewList.querySelectorAll('li');
      expect(rows).toHaveLength(2);
      expect(rows[0].classList.contains('removed')).toBe(true);
      expect(rows[0]).toHaveTextContent('− pass at 1.000s — removed');
      expect(rows[1].classList.contains('removed')).toBe(true);
      expect(rows[1]).toHaveTextContent('− pass at 1:21.000s — removed');
      // Still preview-only — nothing sent.
      expect(sendSpy).not.toHaveBeenCalled();
    });

    it('finishing a threshold DRAG on the graph sends nothing (no phantom "Lap inserted")', async () => {
      // The live bug (2026-07-03): releasing a threshold drag inside the svg makes the browser
      // synthesize a click on it — the old click-to-insert path turned EVERY drag into an
      // immediate InsertLap ruling, before any commit.
      const { session, sendSpy } = renderTune();
      render(Marshaling, { session });

      const svg = screen.getByLabelText(/RSSI trace for ALICE/);
      const handle = screen.getByRole('slider', { name: 'Enter threshold for ALICE' });
      // The full drag gesture (jsdom has no PointerEvent — a MouseEvent with the pointer type)…
      fireEvent(handle, new MouseEvent('pointerdown', { bubbles: true, clientY: 40 }));
      fireEvent(handle, new MouseEvent('pointermove', { bubbles: true, clientY: 30 }));
      fireEvent(handle, new MouseEvent('pointerup', { bubbles: true }));
      // …then the click the browser synthesizes on the svg where the pointer was released.
      await fireEvent.click(svg, { clientX: 500, clientY: 30 });

      // Rulings exist only after an explicit action — the drag+click sent NOTHING.
      expect(sendSpy).not.toHaveBeenCalled();
    });

    it('flags equal/inverted thresholds instead of detecting', async () => {
      const { session } = renderTune();
      render(Marshaling, { session });
      await fireEvent.input(screen.getByLabelText('Enter threshold'), { target: { value: '95' } });
      expect(screen.getByText(/Enter must be above exit/)).toBeInTheDocument();
      expect(
        (screen.getByRole('button', { name: 'Commit re-detection' }) as HTMLButtonElement).disabled
      ).toBe(true);
    });

    it('Commit sends a heat-tagged InsertLap per ADDED pass', async () => {
      const { session, sendSpy } = renderTune();
      render(Marshaling, { session });

      await fireEvent.input(screen.getByLabelText('Enter threshold'), {
        target: { value: '100' }
      });
      await fireEvent.click(screen.getByRole('button', { name: 'Commit re-detection' }));
      // Exactly one command: the added pass at the 60s peak, tagged with the marshaled heat and
      // the adapter from the trace's CompetitorKey.
      await waitFor(() =>
        expect(sendSpy).toHaveBeenCalledWith({
          InsertLap: { adapter: 'rh-1', competitor: 'ALICE', at: 60_000_000, heat: 'heat-1' }
        })
      );
      expect(sendSpy).toHaveBeenCalledTimes(1);
    });

    it('Commit sends a VoidDetection per REMOVED pass (the exact boundary refs)', async () => {
      const { session, sendSpy } = renderTune();
      render(Marshaling, { session });

      // Raise enter above every peak but the 132 one: only the 41s pass survives — the holeshot
      // (ref 10) and the 81s gate pass (ref 14) are removed.
      await fireEvent.input(screen.getByLabelText('Enter threshold'), {
        target: { value: '131' }
      });
      expect(screen.getByTestId('redetect-summary')).toHaveTextContent(
        'Would be 0 laps (+0 added, −2 removed)'
      );
      await fireEvent.click(screen.getByRole('button', { name: 'Commit re-detection' }));
      await waitFor(() => expect(sendSpy).toHaveBeenCalledTimes(2));
      expect(sendSpy).toHaveBeenNthCalledWith(1, { VoidDetection: { target: 10 } });
      expect(sendSpy).toHaveBeenNthCalledWith(2, { VoidDetection: { target: 14 } });
    });

    it('keeps the tune PREVIEW live on a Final heat but locks Commit with the explaining title', async () => {
      // Inspection is fine on an official result — the sliders + preview stay live — but
      // COMMITTING would re-score it, so the button is disabled with the revert-first title.
      const { session, sendSpy } = makeTestSession({
        live: { ...liveRunning, phase: 'Final', lifecycle: 'Official' },
        laps: TUNE_LAPS,
        signal: TUNE_TRACE
      });
      render(Marshaling, { session });

      // The preview still re-detects live at the adjusted level (a dirty +1 diff)…
      await fireEvent.input(screen.getByLabelText('Enter threshold'), {
        target: { value: '100' }
      });
      expect(screen.getByTestId('redetect-summary')).toHaveTextContent(
        'Would be 3 laps (+1 added, −0 removed)'
      );
      // …but Commit is locked, and the title says why.
      const commit = screen.getByRole('button', {
        name: 'Commit re-detection'
      }) as HTMLButtonElement;
      expect(commit.disabled).toBe(true);
      expect(commit.title).toBe('This result is official — Revert it to make corrections.');
      await fireEvent.click(commit);
      expect(sendSpy).not.toHaveBeenCalled();
    });

    it('Reset restores the recorded thresholds and clears the preview diff', async () => {
      const { session, sendSpy } = renderTune();
      render(Marshaling, { session });

      await fireEvent.input(screen.getByLabelText('Enter threshold'), {
        target: { value: '100' }
      });
      expect(screen.getByTestId('redetect-summary')).toHaveTextContent('+1 added');
      await fireEvent.click(screen.getByRole('button', { name: 'Reset' }));
      expect((screen.getByLabelText('Enter threshold') as HTMLInputElement).value).toBe('110');
      expect((screen.getByLabelText('Exit threshold') as HTMLInputElement).value).toBe('95');
      expect(screen.getByTestId('redetect-summary')).toHaveTextContent(
        'Would be 2 laps (+0 added, −0 removed)'
      );
      expect(
        (screen.getByRole('button', { name: 'Commit re-detection' }) as HTMLButtonElement).disabled
      ).toBe(true);
      expect(sendSpy).not.toHaveBeenCalled();
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
