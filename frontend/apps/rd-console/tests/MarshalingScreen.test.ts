import { describe, expect, it } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import Marshaling from '../src/screens/Marshaling.svelte';
import { makeTestSession } from './support.js';
import { liveRunning, lapList, marshalingAudit } from './fixtures.js';

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

    // Select BOB's only lap (end_ref 13).
    await fireEvent.click(screen.getByRole('button', { name: /Lap 1\s*43\.000/ }));
    await fireEvent.input(screen.getByLabelText('Correction time'), { target: { value: '21' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Split' }));
    expect(sendSpy).toHaveBeenCalledWith({ SplitLap: { target: 13, at: 21_000_000 } });
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
    // Kind defaults to Disqualify.
    await fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    expect(sendSpy).toHaveBeenCalledWith({
      ApplyPenalty: { heat: 'heat-1', competitor: 'BOB', penalty: 'Disqualify' }
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

  it('void heat confirms first, then emits VoidHeat', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning, laps: lapList });
    render(Marshaling, { session });

    await fireEvent.click(screen.getByRole('button', { name: 'Void heat' }));
    expect(sendSpy).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    expect(sendSpy).toHaveBeenCalledWith({ VoidHeat: { heat: 'heat-1' } });
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
    // Newest first: the DQ (at_ref 20) precedes the void (at_ref 18).
    expect(entries[0]).toHaveTextContent('DQ applied for CARMEN');
    expect(entries[1]).toHaveTextContent('Detection voided (ref 12)');
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
    expect(screen.getByText('DQ applied for CARMEN')).toBeInTheDocument();
    // No mutating controls.
    expect(screen.queryByRole('button', { name: 'Remove (void)' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Split' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Apply' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Void heat' })).toBeNull();
    expect(screen.queryByLabelText('Reverse ruling')).toBeNull();
  });
});
