/**
 * OpenSeats — check-in on a phone or kiosk: the open seats of the race being flown, who is
 * on each, and Take, Take over and Drop for the signed-in pilot.
 */
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { Pilot } from '@gridfpv/types';
import OpenSeats from '../src/screens/OpenSeats.svelte';
import { buildNames } from '../src/lib/names.js';
import { heat, race, raceSeat } from './competition.js';

const PILOTS: Pilot[] = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] }
];
const names = buildNames([heat('practice', 'Open Practice', ['Open', 'Open', 'p-2b'])], PILOTS);
const ACE = { id: 'p-1a', callsign: 'Ace' };

const flying = (first?: string, second?: string) =>
  race('practice', 1, 'Running', {
    seats: [
      raceSeat(0, undefined, { holder: first }),
      raceSeat(1, undefined, { holder: second }),
      raceSeat(2, 'p-2b')
    ]
  });
const actions = () => ({ take: vi.fn(async () => undefined), drop: vi.fn(async () => undefined) });
const rows = () =>
  within(screen.getByRole('list', { name: 'Open seats' })).getAllByRole('listitem');

describe('OpenSeats', () => {
  it('lists the open seats only, and a free one can be taken', async () => {
    const act = actions();
    render(OpenSeats, { race: flying(), names, pilot: ACE, actions: act });
    // The seat with Bee named on it is not an open seat.
    expect(rows()).toHaveLength(2);
    expect(rows()[0]).toHaveTextContent('Seat 1');
    expect(rows()[0]).toHaveTextContent('Free');
    await fireEvent.click(screen.getByRole('button', { name: 'Take Seat 2' }));
    await waitFor(() => expect(act.take).toHaveBeenCalled());
    expect(act.take).toHaveBeenCalledWith(
      'practice#1',
      expect.objectContaining({ adapter: 'bench', seat: 'seat-2' })
    );
    const text = document.body.textContent ?? '';
    for (const raw of ['practice#1', 'bench', 'seat-1', 'p-1a', 'p-2b']) {
      expect(text).not.toContain(raw);
    }
  });

  it('a seat another pilot has names them, and can be taken over', async () => {
    const act = actions();
    render(OpenSeats, { race: flying('p-2b'), names, pilot: ACE, actions: act });
    expect(rows()[0]).toHaveTextContent('Bee');
    await fireEvent.click(screen.getByRole('button', { name: 'Take Seat 1 over from Bee' }));
    await waitFor(() => expect(act.take).toHaveBeenCalled());
  });

  it("the pilot's own seat is marked, says so, and can be dropped", async () => {
    const act = actions();
    render(OpenSeats, { race: flying(undefined, 'p-1a'), names, pilot: ACE, actions: act });
    expect(rows()[1]).toHaveClass('mine');
    expect(rows()[1]).toHaveTextContent('You');
    expect(screen.getByText(/^You have Seat 2\./)).toBeVisible();
    expect(screen.queryByRole('button', { name: 'Take Seat 2' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Drop Seat 2' }));
    await waitFor(() => expect(act.drop).toHaveBeenCalledWith('practice#1'));
  });

  it("shows a seat's channel when it is known", () => {
    render(OpenSeats, {
      race: flying(),
      names,
      pilot: ACE,
      actions: actions(),
      channelOf: (seat) => (seat.seat === 'seat-1' ? 'R1 5658' : undefined)
    });
    expect(rows()[0]).toHaveTextContent('R1 5658');
    expect(rows()[1]).not.toHaveTextContent('5658');
  });

  it("shows the Director's reason when it refuses, and offers no buttons to someone only looking", async () => {
    const SAID = 'cannot take a seat in race 1 of Open Practice while it is Stopped';
    const take = vi.fn(async () => Promise.reject(new Error(SAID)));
    const { unmount } = render(OpenSeats, {
      race: flying(),
      names,
      pilot: ACE,
      actions: { take, drop: vi.fn() }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Take Seat 1' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(SAID);
    unmount();
    render(OpenSeats, { race: flying(), names, pilot: ACE });
    expect(screen.queryByRole('button')).toBeNull();
  });
});
