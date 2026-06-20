import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import Marshaling from '../src/screens/Marshaling.svelte';
import { makeTestSession } from './support.js';
import { liveRunning, lapList } from './fixtures.js';

describe('Marshaling', () => {
  it('renders the per-competitor lap list', () => {
    const { session } = makeTestSession({ live: liveRunning });
    render(Marshaling, { session, laps: lapList });
    expect(screen.getByText('Lap 1: 41.000')).toBeInTheDocument();
  });

  it('emits a VoidHeat command after confirming the destructive action', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning });
    render(Marshaling, { session });

    await fireEvent.click(screen.getByRole('button', { name: 'Void heat' }));
    expect(sendSpy).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    expect(sendSpy).toHaveBeenCalledWith({ VoidHeat: { heat: 'heat-1' } });
  });

  it('emits an ApplyPenalty (time added) command', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning });
    render(Marshaling, { session });

    await fireEvent.change(screen.getByLabelText('Penalty competitor'), {
      target: { value: 'BOB' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Apply penalty' }));

    expect(sendSpy).toHaveBeenCalledWith({
      ApplyPenalty: {
        heat: 'heat-1',
        competitor: 'BOB',
        penalty: { TimeAdded: { micros: 2_000_000 } }
      }
    });
  });

  it('emits a VoidDetection command for a log offset', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning });
    render(Marshaling, { session });

    await fireEvent.click(screen.getByRole('button', { name: 'Void detection' }));
    expect(sendSpy).toHaveBeenCalledWith({ VoidDetection: { target: 0 } });
  });
});
