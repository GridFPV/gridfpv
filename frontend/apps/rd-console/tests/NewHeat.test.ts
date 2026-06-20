import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import NewHeat from '../src/screens/NewHeat.svelte';
import { makeTestSession } from './support.js';
import { failAck } from './fixtures.js';

describe('NewHeat', () => {
  it('emits a Register per pilot then a ScheduleHeat from the typed field', async () => {
    const { session, sendSpy } = makeTestSession();
    render(NewHeat, { session });

    // Heat id seeds to 'q-1'; fill the two default pilot rows.
    const pilots = screen.getAllByLabelText(/Pilot \d name/) as HTMLInputElement[];
    await fireEvent.input(pilots[0], { target: { value: 'Ace' } });
    await fireEvent.input(pilots[1], { target: { value: 'Bee' } });

    await fireEvent.click(screen.getByRole('button', { name: 'Schedule heat' }));

    // The form sends the batch across awaits; wait for all three to land.
    await waitFor(() => expect(sendSpy).toHaveBeenCalledTimes(3));
    expect(sendSpy.mock.calls.map((c) => c[0])).toEqual([
      { Register: { adapter: 'sim', competitor: 'Ace', pilot: 'Ace' } },
      { Register: { adapter: 'sim', competitor: 'Bee', pilot: 'Bee' } },
      { ScheduleHeat: { heat: 'q-1', lineup: ['Ace', 'Bee'] } }
    ]);
  });

  it('disables scheduling until at least two pilots are named', () => {
    const { session } = makeTestSession();
    render(NewHeat, { session });
    expect(
      (screen.getByRole('button', { name: 'Schedule heat' }) as HTMLButtonElement).disabled
    ).toBe(true);
  });

  it('stops the batch on a failed ack (no ScheduleHeat after a failed Register)', async () => {
    const { session, sendSpy } = makeTestSession({ ack: failAck });
    render(NewHeat, { session });
    const pilots = screen.getAllByLabelText(/Pilot \d name/) as HTMLInputElement[];
    await fireEvent.input(pilots[0], { target: { value: 'Ace' } });
    await fireEvent.input(pilots[1], { target: { value: 'Bee' } });

    await fireEvent.click(screen.getByRole('button', { name: 'Schedule heat' }));

    // Only the first Register was attempted; the batch aborted before ScheduleHeat. Settle
    // briefly to prove no further command is sent after the failed ack.
    await waitFor(() => expect(sendSpy).toHaveBeenCalled());
    expect(sendSpy).toHaveBeenCalledTimes(1);
    expect(sendSpy.mock.calls[0][0]).toEqual({
      Register: { adapter: 'sim', competitor: 'Ace', pilot: 'Ace' }
    });
  });
});
