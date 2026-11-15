/**
 * The rulings made on the Phases page, in the phase tree: lock and unlock a phase, reopen a
 * heat, set and clear a position, disqualify and reinstate, the proposed disqualification,
 * and removing a phase nothing was raced in. Rendered against a stand-in live store.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { Command, EventView, Pilot } from '@gridfpv/types';
import PhasesPage from '../src/screens/PhasesPage.svelte';
import type { LiveSession } from '../src/lib/eventLive.svelte.js';
import { eventView, group, heat, race } from './competition.js';
import { openLive } from './support.js';

const PILOTS: Pilot[] = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] },
  { id: 'p-3c', callsign: 'Cat', vtx_types: [] }
];

const placed = (pilot: string, position: number, over = {}) => ({
  pilot,
  position,
  disqualified: false,
  position_set: false,
  flew: true,
  wins: 0,
  points: 0,
  laps: 3,
  ...over
});

async function open(view: EventView, over: Partial<LiveSession> = {}) {
  const { session, live } = await openLive(view, {
    listPilots: vi.fn(async () => PILOTS),
    ...over
  });
  const onrun = vi.fn();
  render(PhasesPage, { live, onrun });
  return { session, onrun };
}

const sent = (session: LiveSession): Command[] =>
  vi.mocked(session.sendCommand).mock.calls.map((call) => call[1]);

/** Every raw handle in these fixtures. None may be on the page. */
function expectNoRawIds() {
  const text = document.body.textContent ?? '';
  for (const raw of ['finals', 'q-1', 'q-2', '#1', 'bench', 'seat-1', 'p-1a', 'p-2b', 'fmt-']) {
    expect(text).not.toContain(raw);
  }
}

describe('heat and phase rulings', () => {
  const phases = (over = {}, top = {}) => [
    group('finals', 'Finals', undefined, {
      ranking: [placed('p-1a', 1), placed('p-2b', 2)],
      ...top
    }),
    heat('q-1', 'Final Heat 1', ['p-1a', 'p-2b'], {
      parent: 'finals',
      complete: true,
      director_done: true,
      ranking: [placed('p-1a', 1, { position_set: true }), placed('p-2b', 2)],
      ...over
    })
  ];

  it('locks a phase, and unlocks only the phase that carries the lock', async () => {
    const first = await open(eventView({ phases: phases() }));
    await fireEvent.click(screen.getByRole('button', { name: 'Lock Finals' }));
    await waitFor(() =>
      expect(sent(first.session)).toEqual([{ Record: { PhaseLocked: { phase: 'finals' } } }])
    );
    document.body.innerHTML = '';

    const second = await open(
      eventView({ phases: phases({ locked: true }, { locked: true, locked_here: true }) })
    );
    // The heat is locked from above: it offers neither Lock nor Unlock, and no ruling.
    expect(screen.queryByRole('button', { name: /Lock Final Heat 1/ })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Unlock Final Heat 1' })).toBeNull();
    expect(screen.queryByRole('button', { name: /Set the position/ })).toBeNull();
    expect(screen.queryByRole('button', { name: /Disqualify/ })).toBeNull();
    expect(screen.queryByRole('button', { name: /Reopen/ })).toBeNull();
    expect(screen.getAllByText(/The results are official/).length).toBeGreaterThan(0);
    await fireEvent.click(screen.getByRole('button', { name: 'Unlock Finals' }));
    await waitFor(() =>
      expect(sent(second.session)).toEqual([{ Record: { PhaseUnlocked: { phase: 'finals' } } }])
    );
  });

  it('reopens a heat the director called done', async () => {
    const { session } = await open(eventView({ phases: phases() }));
    await fireEvent.click(screen.getByRole('button', { name: 'Reopen Final Heat 1' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([{ Record: { HeatReopened: { heat: 'q-1' } } }])
    );
  });

  it('sets a pilot position in a heat with a note, marks hand-set ones, and clears them', async () => {
    const { session } = await open(eventView({ phases: phases() }));
    const ranking = screen.getByRole('list', { name: 'Ranking of Final Heat 1' });
    const rows = within(ranking).getAllByRole('listitem');
    expect(rows[0]).toHaveTextContent('Ace');
    expect(rows[0]).toHaveTextContent('Set by hand');
    expect(rows[1]).not.toHaveTextContent('Set by hand');
    // Positions are set in a heat, never in a phase above it.
    expect(screen.queryByRole('button', { name: /Set the position of Ace in Finals/ })).toBeNull();

    await fireEvent.click(
      screen.getByRole('button', { name: 'Set the position of Bee in Final Heat 1' })
    );
    const form = await screen.findByRole('form', { name: "Set Bee's position" });
    expect(within(form).getByLabelText('Position')).toHaveValue('2');
    await fireEvent.change(within(form).getByLabelText('Position'), { target: { value: '1' } });
    await fireEvent.input(within(form).getByLabelText('Note'), {
      target: { value: 'Video review' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Set position' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([
        {
          Record: {
            PositionSet: { heat: 'q-1', pilot: 'p-2b', position: 1, note: 'Video review' }
          }
        }
      ])
    );
    await waitFor(() =>
      expect(screen.queryByRole('form', { name: "Set Bee's position" })).toBeNull()
    );

    await fireEvent.click(
      screen.getByRole('button', { name: 'Clear the position of Ace in Final Heat 1' })
    );
    await waitFor(() =>
      expect(sent(session)[1]).toEqual({
        Record: { PositionCleared: { heat: 'q-1', pilot: 'p-1a' } }
      })
    );
    expectNoRawIds();
  });

  it('disqualifies from a phase through the dialog: the next pilot takes the place by default', async () => {
    const { session } = await open(eventView({ phases: phases() }));
    await fireEvent.click(screen.getByRole('button', { name: 'Disqualify Bee from Finals' }));
    const form = await screen.findByRole('form', { name: 'Disqualify Bee' });
    expect(within(form).getByLabelText('The next pilot takes their place')).toBeChecked();
    await fireEvent.click(screen.getByRole('button', { name: 'Disqualify' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([
        { Record: { Disqualified: { phase: 'finals', pilot: 'p-2b', keep_place_empty: false } } }
      ])
    );
    await waitFor(() => expect(screen.queryByRole('form', { name: 'Disqualify Bee' })).toBeNull());
  });

  it('can keep the place empty and give a reason; a refusal stays in the dialog', async () => {
    const SAID = 'Finals is locked. Unlock it first.';
    const sendCommand = vi
      .fn<LiveSession['sendCommand']>()
      .mockRejectedValueOnce(Object.assign(new Error(SAID), { status: 409, code: 'Refused' }))
      .mockResolvedValue({ cursor: 1 });
    const { session } = await open(eventView({ phases: phases() }), { sendCommand });
    await fireEvent.click(screen.getByRole('button', { name: 'Disqualify Ace from Finals' }));
    const form = await screen.findByRole('form', { name: 'Disqualify Ace' });
    await fireEvent.input(within(form).getByLabelText('Reason'), {
      target: { value: 'Two false starts' }
    });
    await fireEvent.click(within(form).getByLabelText('Keep their place empty'));
    await fireEvent.click(screen.getByRole('button', { name: 'Disqualify' }));
    expect(await within(form).findByText(SAID)).toBeVisible();
    await fireEvent.click(screen.getByRole('button', { name: 'Disqualify' }));
    await waitFor(() => expect(sent(session)).toHaveLength(2));
    expect(sent(session)[1]).toEqual({
      Record: {
        Disqualified: {
          phase: 'finals',
          pilot: 'p-1a',
          reason: 'Two false starts',
          keep_place_empty: true
        }
      }
    });
  });

  it('reinstates a disqualified pilot', async () => {
    const { session } = await open(
      eventView({
        phases: phases(
          {},
          { ranking: [placed('p-1a', 1), placed('p-2b', 2, { disqualified: true })] }
        )
      })
    );
    const row = within(screen.getByRole('list', { name: 'Ranking of Finals' })).getAllByRole(
      'listitem'
    )[1];
    expect(row).toHaveTextContent('Disqualified');
    await fireEvent.click(screen.getByRole('button', { name: 'Reinstate Bee in Finals' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([
        { Record: { Reinstated: { phase: 'finals', pilot: 'p-2b' } } }
      ])
    );
  });

  it('shows a proposed disqualification as a notice; nothing is sent until the director decides', async () => {
    const { session } = await open(
      eventView({
        phases: phases(),
        proposed_disqualifications: [{ phase: 'finals', pilot: 'p-2b', false_starts: 2, limit: 2 }]
      })
    );
    const notice = screen.getByText('Bee has 2 false starts in Finals. The limit is 2.');
    expect(notice).toBeVisible();
    expect(session.sendCommand).not.toHaveBeenCalled();
    const alert = notice.closest('[role="alert"]') as HTMLElement;
    await fireEvent.click(
      within(alert).getByRole('button', { name: 'Disqualify Bee from Finals' })
    );
    expect(await screen.findByRole('form', { name: 'Disqualify Bee' })).toBeVisible();
    expect(session.sendCommand).not.toHaveBeenCalled();
  });

  it("a refused ruling is shown in the Director's words", async () => {
    const SAID = 'Finals holds an open protest. Resolve it before locking.';
    await open(eventView({ phases: phases() }), {
      sendCommand: vi.fn(async () =>
        Promise.reject(Object.assign(new Error(SAID), { status: 409, code: 'Refused' }))
      )
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Lock Finals' }));
    expect(await screen.findByText(SAID)).toBeVisible();
  });
});

describe('removing a phase', () => {
  const rowOf = (label: string): HTMLElement =>
    within(screen.getByRole('list', { name: 'Phases' }))
      .getAllByRole('listitem')
      .filter((item) => item.classList.contains('phase'))
      .find((item) => item.querySelector('.label')?.textContent === label) as HTMLElement;

  const view = () =>
    eventView({
      phases: [
        group('quali', 'Qualifying'),
        heat('q-1', 'Qualifying Heat 1', ['p-1a'], { parent: 'quali' }),
        heat('q-2', 'Qualifying Heat 2', ['p-2b'], { parent: 'quali' }),
        group('finals', 'Finals'),
        heat('f-1', 'Final', ['p-1a'], { locked: true })
      ],
      races: [race('q-1', 1, 'Saved')]
    });

  it('offers Remove only where nothing is beneath, nothing was raced and nothing is locked', async () => {
    await open(view());
    const offers = (label: string) =>
      within(rowOf(label)).queryByRole('button', { name: /^Remove / }) !== null;
    // Qualifying holds heats; Heat 1 has a race; the Final is locked.
    expect(offers('Qualifying')).toBe(false);
    expect(offers('Qualifying Heat 1')).toBe(false);
    expect(offers('Final')).toBe(false);
    // Heat 2 was never raced, and Finals holds nothing.
    expect(offers('Qualifying Heat 2')).toBe(true);
    expect(offers('Finals')).toBe(true);
  });

  it('asks first: the first click sends nothing, and Cancel backs out', async () => {
    const { session } = await open(view());
    const row = rowOf('Qualifying Heat 2');
    await fireEvent.click(within(row).getByRole('button', { name: /^Remove / }));
    expect(session.sendCommand).not.toHaveBeenCalled();
    await fireEvent.click(within(row).getByRole('button', { name: 'Cancel' }));
    expect(within(row).getByRole('button', { name: /^Remove / })).toBeVisible();
    expect(session.sendCommand).not.toHaveBeenCalled();
  });

  it('sends PhaseRemoved for that phase once confirmed', async () => {
    const { session } = await open(view());
    const row = rowOf('Qualifying Heat 2');
    await fireEvent.click(within(row).getByRole('button', { name: /^Remove / }));
    await fireEvent.click(within(row).getByRole('button', { name: 'Confirm' }));
    await waitFor(() =>
      expect(sent(session)).toEqual([{ Record: { PhaseRemoved: { phase: 'q-2' } } }])
    );
  });

  it("shows the Director's sentence when it refuses to remove", async () => {
    const SAID = 'Finals is counted by a class. Take it out of the standings first.';
    await open(view(), {
      sendCommand: vi.fn(async () =>
        Promise.reject(Object.assign(new Error(SAID), { status: 409, code: 'Refused' }))
      )
    });
    const row = rowOf('Finals');
    await fireEvent.click(within(row).getByRole('button', { name: /^Remove / }));
    await fireEvent.click(within(row).getByRole('button', { name: 'Confirm' }));
    expect(await screen.findByText(SAID)).toBeVisible();
  });
});
