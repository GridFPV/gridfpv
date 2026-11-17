/**
 * The sign-up sheet on a phone: what everyone sees, and what a signed-in pilot can press.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { SignupView } from '@gridfpv/types';
import PilotSignup from '../src/screens/PilotSignup.svelte';
import {
  SIGNUP_RULES,
  signupEvent,
  signupHeat,
  signupNames,
  signupPhase,
  signupRuns,
  signupSheet
} from './signupFixtures.js';

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const BEE = { id: 'p-bee', callsign: 'Bee' };
const ACE = { id: 'p-ace', callsign: 'Ace' };

function open(
  sheet: SignupView = signupSheet(),
  /** `null` is somebody who is not signed in. */
  who: { id: string; callsign: string } | null = BEE,
  withActions = true
) {
  const pilot = who ?? undefined;
  const actions = {
    take: vi.fn(async (_heat: string, _seat: { adapter: string; seat: string }) => {}),
    next: vi.fn(async (_phase: string, _seat: { adapter: string; seat: string }) => {}),
    leave: vi.fn(async (_heat: string) => {})
  };
  render(PilotSignup, {
    phases: [signupPhase(sheet)],
    names: signupNames(signupEvent(sheet)),
    pilot,
    channels: [{ adapter: 'gate', seat: '0', freq_mhz: 5658 }],
    actions: withActions ? actions : undefined
  });
  return actions;
}

const heatItem = (n: number) => screen.getByRole('listitem', { name: `Qualifying Heat ${n}` });
const seatRow = (n: number, seat: number) => within(heatItem(n)).getAllByRole('listitem')[seat];

describe('PilotSignup: what everyone sees', () => {
  it('lists each heat by number with every seat, its channel, and who is on it', () => {
    open(signupSheet(), null, false);
    const section = screen.getByRole('region', { name: 'Sign-up for Qualifying' });
    expect(within(section).getByRole('heading')).toHaveTextContent('Qualifying');
    expect(within(heatItem(1)).getByText('Heat 1')).toBeVisible();
    expect(seatRow(1, 0)).toHaveTextContent('Seat 1');
    expect(seatRow(1, 0)).toHaveTextContent('R1 · 5658 MHz');
    expect(seatRow(1, 0)).toHaveTextContent('Ace');
    expect(seatRow(1, 1)).toHaveTextContent('Free');
    // A seat with no channel known shows none.
    expect(seatRow(1, 1)).not.toHaveTextContent('MHz');
    // No raw id anywhere.
    expect(document.body.textContent).not.toContain('p-ace');
    expect(document.body.textContent).not.toContain('quali/heat');
  });

  it('someone who is not signed in sees no button and is told how to get one', () => {
    open(signupSheet(), null, false);
    expect(screen.queryAllByRole('button')).toEqual([]);
    expect(screen.getByText('Sign in as a pilot to take a seat.')).toBeVisible();
  });

  it('says whether sign-up is open, and what state each heat is in', () => {
    open(
      signupSheet({
        rules: { ...SIGNUP_RULES, open: false },
        heats: [
          signupHeat(1, ['p-ace', 'p-cat', null], 'Flown'),
          signupHeat(2, ['p-ace', null, null], 'Racing'),
          signupHeat(3, [null, null, null], 'Closed'),
          signupHeat(4, ['p-ace', 'p-cat', 'p-bee']),
          signupHeat(5, [null, null, null])
        ]
      }),
      null,
      false
    );
    expect(screen.getByRole('region', { name: 'Sign-up for Qualifying' })).toHaveTextContent(
      'Closed'
    );
    expect(heatItem(1)).toHaveTextContent('Flown');
    expect(heatItem(2)).toHaveTextContent('On now');
    expect(heatItem(3)).toHaveTextContent('Closed');
    expect(heatItem(4)).toHaveTextContent('Full');
    expect(within(heatItem(5)).queryByText(/Full|Closed|Flown|On now/)).toBeNull();
  });

  it('says so when there is nothing to sign up for', () => {
    render(PilotSignup, { phases: [], names: signupNames(signupEvent()) });
    expect(screen.getByText('There is nothing to sign up for right now.')).toBeVisible();
  });
});

describe('PilotSignup: a signed-in pilot', () => {
  it('takes a free seat', async () => {
    const actions = open();
    await fireEvent.click(
      within(heatItem(2)).getByRole('button', { name: 'Take Seat 2 in Qualifying Heat 2' })
    );
    expect(actions.take).toHaveBeenCalledWith('quali/heat-2', { adapter: 'gate', seat: '1' });
  });

  it('leaves their own seat, and sees it marked as theirs', async () => {
    const actions = open(signupSheet(), ACE);
    expect(seatRow(1, 0)).toHaveClass('mine');
    await fireEvent.click(
      within(heatItem(1)).getByRole('button', { name: 'Leave Qualifying Heat 1' })
    );
    expect(actions.leave).toHaveBeenCalledWith('quali/heat-1');
    // The other seats of that heat are not offered: they are already in it.
    expect(within(heatItem(1)).queryByRole('button', { name: /^Take/ })).toBeNull();
    expect(seatRow(1, 1)).toHaveTextContent('You are in this heat');
  });

  it("has no button on another pilot's seat", () => {
    open();
    expect(within(seatRow(1, 0)).queryByRole('button')).toBeNull();
    expect(seatRow(1, 0)).toHaveTextContent('Ace');
  });

  it('shows how many runs they have had and have left', () => {
    open(
      signupSheet({
        rules: { ...SIGNUP_RULES, run_limit: 3 },
        pilots: [signupRuns('p-bee', { used: 1, held: 1, allowed: 3 })]
      })
    );
    expect(screen.getByTestId('my-runs')).toHaveTextContent('1 flown · 1 held · 1 left of 3');
  });

  it('says why a free seat cannot be taken, and offers no button for it', () => {
    open(
      signupSheet({
        rules: { ...SIGNUP_RULES, run_limit: 1 },
        pilots: [signupRuns('p-bee', { used: 1, allowed: 1 })]
      })
    );
    expect(seatRow(2, 0)).toHaveTextContent('No runs left');
    expect(screen.queryByRole('button', { name: /^Take/ })).toBeNull();
    // And the next-free buttons are there but cannot be pressed.
    for (const button of within(
      screen.getByRole('group', { name: 'Next free spot in Qualifying' })
    ).getAllByRole('button')) {
      expect(button).toBeDisabled();
    }
  });

  it('cannot take a seat while sign-up is closed, but can still leave theirs', () => {
    open(signupSheet({ rules: { ...SIGNUP_RULES, open: false } }), ACE);
    expect(seatRow(2, 0)).toHaveTextContent('Sign-up is closed');
    expect(screen.queryByRole('button', { name: /^Take/ })).toBeNull();
    expect(screen.getByRole('button', { name: 'Leave Qualifying Heat 1' })).toBeEnabled();
  });

  it('in a heat that is closed or on, their own seat says "You" with nothing to press', () => {
    for (const state of ['Closed', 'Racing', 'Flown'] as const) {
      open(signupSheet({ heats: [signupHeat(1, ['p-ace', null, null], state)] }), ACE);
      expect(seatRow(1, 0)).toHaveTextContent('You');
      expect(within(heatItem(1)).queryAllByRole('button')).toEqual([]);
      cleanup();
    }
  });

  it('a pilot who is not in the phase is told so', () => {
    open(signupSheet(), { id: 'p-stranger', callsign: 'Stranger' });
    expect(screen.getByTestId('my-runs')).toHaveTextContent('You are not in this phase.');
    expect(screen.queryByRole('button', { name: /^Take/ })).toBeNull();
  });

  it('takes the next free spot on a seat, and only where there is one', async () => {
    const actions = open(
      signupSheet({
        heats: [
          signupHeat(1, ['p-ace', null, null]),
          signupHeat(2, ['p-cat', null, null]),
          signupHeat(3, ['p-ace', null, null])
        ]
      })
    );
    const group = within(screen.getByRole('group', { name: 'Next free spot in Qualifying' }));
    // Seat 1 is taken in every heat.
    expect(group.getByRole('button', { name: 'Next free spot on Seat 1' })).toBeDisabled();
    await fireEvent.click(group.getByRole('button', { name: 'Next free spot on Seat 2' }));
    expect(actions.next).toHaveBeenCalledWith('quali', { adapter: 'gate', seat: '1' });
  });

  it("shows the Director's reason when it refuses, and clears it on the next try", async () => {
    const actions = open();
    actions.take.mockRejectedValueOnce(
      new Error('that seat in Qualifying Heat 2 is already taken')
    );
    const take = within(heatItem(2)).getByRole('button', {
      name: 'Take Seat 1 in Qualifying Heat 2'
    });
    await fireEvent.click(take);
    expect(await screen.findByTestId('signup-error')).toHaveTextContent(
      'that seat in Qualifying Heat 2 is already taken'
    );
    await fireEvent.click(take);
    await waitFor(() => expect(screen.queryByTestId('signup-error')).toBeNull());
  });

  it('sends one request however fast the button is tapped', async () => {
    const actions = open();
    let finish: () => void = () => {};
    actions.take.mockImplementationOnce(() => new Promise<void>((resolve) => (finish = resolve)));
    const take = within(heatItem(2)).getByRole('button', {
      name: 'Take Seat 1 in Qualifying Heat 2'
    });
    await fireEvent.click(take);
    await fireEvent.click(take);
    await fireEvent.click(
      within(heatItem(3)).getByRole('button', { name: 'Take Seat 1 in Qualifying Heat 3' })
    );
    expect(actions.take).toHaveBeenCalledOnce();
    finish();
  });

  it('a signed-in pilot with nothing to act through can only look', () => {
    open(signupSheet(), BEE, false);
    expect(screen.queryAllByRole('button')).toEqual([]);
  });

  it('shows every sign-up phase of the event', () => {
    render(PilotSignup, {
      phases: [signupPhase(), { ...signupPhase(), id: 'practice', name: 'Practice' }],
      names: signupNames(signupEvent()),
      pilot: BEE
    });
    expect(screen.getByRole('region', { name: 'Sign-up for Qualifying' })).toBeVisible();
    expect(screen.getByRole('region', { name: 'Sign-up for Practice' })).toBeVisible();
  });
});

describe('PilotSignup: the grid for a kiosk', () => {
  function openGrid(sheet: SignupView = signupSheet(), who: typeof BEE | null = BEE) {
    const actions = {
      take: vi.fn(async (_heat: string, _seat: { adapter: string; seat: string }) => {}),
      next: vi.fn(async (_phase: string, _seat: { adapter: string; seat: string }) => {}),
      leave: vi.fn(async (_heat: string) => {})
    };
    render(PilotSignup, {
      phases: [signupPhase(sheet)],
      names: signupNames(signupEvent(sheet)),
      pilot: who ?? undefined,
      channels: [{ adapter: 'gate', seat: '0', freq_mhz: 5658 }],
      actions: who ? actions : undefined,
      layout: 'grid'
    });
    return actions;
  }
  const row = (n: number) => screen.getByRole('row', { name: `Qualifying Heat ${n}` });
  const cells = (n: number) => within(row(n)).getAllByRole('cell');

  it('is a table: a row for each heat and a column for each seat, with the channel', () => {
    openGrid();
    const table = screen.getByRole('table', { name: 'Heats of Qualifying' });
    const headers = within(table)
      .getAllByRole('columnheader')
      .map((cell) => cell.textContent?.replace(/\s+/g, ' ').trim());
    expect(headers).toEqual([
      'Heat',
      'Seat 1 R1 · 5658 MHz Next free',
      'Seat 2 Next free',
      'Seat 3 Next free'
    ]);
    expect(within(table).getAllByRole('row')).toHaveLength(4);
    expect(cells(1)[0]).toHaveTextContent('Ace');
    // The phone's list is not drawn as well.
    expect(screen.queryByRole('listitem', { name: 'Qualifying Heat 1' })).toBeNull();
    expect(screen.queryByRole('group', { name: /Next free spot/ })).toBeNull();
    expect(document.body.textContent).not.toContain('quali/heat');
  });

  it('takes a free seat, leaves their own, and takes the next free from a column', async () => {
    const actions = openGrid(signupSheet(), ACE);
    await fireEvent.click(
      within(cells(2)[1]).getByRole('button', { name: 'Take Seat 2 in Qualifying Heat 2' })
    );
    expect(actions.take).toHaveBeenCalledWith('quali/heat-2', { adapter: 'gate', seat: '1' });
    expect(cells(1)[0]).toHaveClass('mine');
    await fireEvent.click(
      within(cells(1)[0]).getByRole('button', { name: 'Leave Qualifying Heat 1' })
    );
    expect(actions.leave).toHaveBeenCalledWith('quali/heat-1');
    await fireEvent.click(screen.getByRole('button', { name: 'Next free spot on Seat 3' }));
    expect(actions.next).toHaveBeenCalledWith('quali', { adapter: 'gate', seat: '2' });
  });

  it("has no button on another pilot's seat, and says why a free seat cannot be taken", () => {
    openGrid(
      signupSheet({
        rules: { ...SIGNUP_RULES, run_limit: 1 },
        pilots: [signupRuns('p-bee', { used: 1, allowed: 1 })]
      })
    );
    expect(within(cells(1)[0]).queryByRole('button')).toBeNull();
    expect(cells(2)[0]).toHaveTextContent('Free');
    expect(cells(2)[0]).toHaveTextContent('No runs left');
    expect(screen.queryByRole('button', { name: /^Take/ })).toBeNull();
    for (const button of screen.getAllByRole('button', { name: /^Next free spot/ })) {
      expect(button).toBeDisabled();
    }
  });

  it('marks the state of each heat, and offers nothing in one that is on or flown', () => {
    openGrid(
      signupSheet({
        heats: [
          signupHeat(1, ['p-bee', null, null], 'Flown'),
          signupHeat(2, ['p-bee', null, null], 'Racing'),
          signupHeat(3, [null, null, null], 'Closed')
        ]
      })
    );
    expect(row(1)).toHaveTextContent('Flown');
    expect(row(2)).toHaveTextContent('On now');
    expect(row(3)).toHaveTextContent('Closed');
    for (const n of [1, 2, 3]) expect(within(row(n)).queryAllByRole('button')).toEqual([]);
  });

  it('with nobody signed in it shows who is where and nothing to press', () => {
    openGrid(signupSheet(), null);
    expect(cells(1)[0]).toHaveTextContent('Ace');
    expect(screen.queryAllByRole('button')).toEqual([]);
  });

  it("shows the Director's reason when it refuses", async () => {
    const actions = openGrid();
    actions.take.mockRejectedValueOnce(
      new Error('that seat in Qualifying Heat 2 is already taken')
    );
    await fireEvent.click(screen.getByRole('button', { name: 'Take Seat 1 in Qualifying Heat 2' }));
    expect(await screen.findByTestId('signup-error')).toHaveTextContent('already taken');
  });
});
