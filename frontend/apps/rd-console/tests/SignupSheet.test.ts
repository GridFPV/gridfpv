/**
 * The director's sign-up sheet: every change is one command.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { Command, SignupView } from '@gridfpv/types';
import SignupSheet from '../src/screens/SignupSheet.svelte';
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

function open(sheet: SignupView = signupSheet(), locked = false) {
  const send = vi.fn(async (_command: Command): Promise<unknown> => true);
  const onrules = vi.fn();
  render(SignupSheet, {
    phase: signupPhase(sheet, locked),
    names: signupNames(signupEvent(sheet)),
    channels: [{ adapter: 'gate', seat: '0', freq_mhz: 5658 }],
    send,
    onrules
  });
  return { send, onrules };
}

const heats = () => screen.getByRole('table', { name: 'Heats of Qualifying' });
const heatRow = (n: number) => within(heats()).getAllByRole('row')[n];
const runs = () => screen.getByRole('table', { name: 'Runs of each pilot in Qualifying' });
const sent = (send: ReturnType<typeof open>['send']) => send.mock.calls.map((call) => call[0]);

describe('SignupSheet', () => {
  it('shows a row for each heat and a column for each seat, with channels and callsigns', () => {
    open();
    const headers = within(heats())
      .getAllByRole('columnheader')
      .map((cell) => cell.textContent?.replace(/\s+/g, ' ').trim());
    expect(headers.slice(0, 4)).toEqual(['Heat', 'Seat 1 R1 · 5658 MHz', 'Seat 2', 'Seat 3']);
    expect(heatRow(1)).toHaveTextContent('Ace');
    expect(document.body.textContent).not.toContain('p-ace');
    expect(document.body.textContent).not.toContain('quali/heat');
  });

  it('opens and closes sign-up, keeping every other rule', async () => {
    const { send } = open(signupSheet({ rules: { ...SIGNUP_RULES, run_limit: 2, open: false } }));
    expect(screen.getByTestId('signup-open')).toHaveTextContent('Closed to pilots');
    await fireEvent.click(screen.getByRole('button', { name: 'Open sign-up' }));
    expect(sent(send)).toEqual([
      {
        Signup: {
          Set: { phase: 'quali', rules: { ...SIGNUP_RULES, run_limit: 2, open: true } }
        }
      }
    ]);
    cleanup();
    const again = open();
    expect(screen.getByTestId('signup-open')).toHaveTextContent('Open to pilots');
    await fireEvent.click(screen.getByRole('button', { name: 'Close sign-up' }));
    expect(sent(again.send)[0]).toMatchObject({
      Signup: { Set: { rules: { open: false } } }
    });
  });

  it('opens the rules', async () => {
    const { onrules } = open();
    await fireEvent.click(screen.getByRole('button', { name: 'Rules' }));
    expect(onrules).toHaveBeenCalledOnce();
  });

  it('adds a pilot to a free seat, offering only pilots who are not in that heat', async () => {
    const { send } = open();
    await fireEvent.click(
      screen.getByRole('button', { name: 'Add a pilot to Seat 2 of Qualifying Heat 1' })
    );
    const dialog = screen.getByRole('dialog');
    expect(within(dialog).getByRole('heading')).toHaveTextContent(
      'Add a pilot: Qualifying Heat 1, Seat 2'
    );
    const offered = within(within(dialog).getByRole('list', { name: 'Pilots to add' }))
      .getAllByRole('button')
      .map((button) => button.textContent?.trim());
    expect(offered).toEqual(['Bee', 'Cat']);
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Bee' }));
    expect(sent(send)).toEqual([
      {
        Signup: {
          Take: {
            heat: 'quali/heat-1',
            seat: { adapter: 'gate', seat: '1' },
            pilot: 'p-bee',
            by: 'Director'
          }
        }
      }
    ]);
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  });

  it('keeps the picker open and says why when adding is refused', async () => {
    const { send } = open();
    send.mockRejectedValueOnce(new Error('that seat in Qualifying Heat 1 is already taken'));
    await fireEvent.click(
      screen.getByRole('button', { name: 'Add a pilot to Seat 2 of Qualifying Heat 1' })
    );
    await fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Bee' }));
    expect(await within(screen.getByRole('dialog')).findByText(/already taken/)).toBeVisible();
  });

  it('asks whether a drop counts as a run, and sends the answer', async () => {
    for (const [button, counted] of [
      ['Does not count', false],
      ['Counts as a run', true]
    ] as const) {
      const { send } = open();
      await fireEvent.click(
        screen.getByRole('button', { name: 'Drop Ace from Qualifying Heat 1' })
      );
      const dialog = screen.getByRole('dialog');
      expect(within(dialog).getByRole('heading')).toHaveTextContent(
        'Drop Ace from Qualifying Heat 1?'
      );
      await fireEvent.click(within(dialog).getByRole('button', { name: button }));
      expect(sent(send)).toEqual([
        { Signup: { Drop: { heat: 'quali/heat-1', pilot: 'p-ace', counted } } }
      ]);
      cleanup();
    }
  });

  it('cancelling a drop sends nothing', async () => {
    const { send } = open();
    await fireEvent.click(screen.getByRole('button', { name: 'Drop Ace from Qualifying Heat 1' }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Cancel' })
    );
    expect(send).not.toHaveBeenCalled();
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('offers to pull a pilot forward only where their seat is free in an earlier heat', async () => {
    const { send } = open(
      signupSheet({
        heats: [
          signupHeat(1, ['p-ace', null, null]),
          signupHeat(2, [null, null, null]),
          signupHeat(3, ['p-cat', 'p-bee', null])
        ]
      })
    );
    // Bee is on seat 2 of heat 3; seat 2 of heat 1 is free.
    await fireEvent.click(screen.getByRole('button', { name: 'Pull Bee forward to heat 1' }));
    expect(sent(send)).toEqual([
      { Signup: { Pull: { pilot: 'p-bee', from: 'quali/heat-3', to: 'quali/heat-1' } } }
    ]);
    // Cat is on seat 1 of heat 3; seat 1 of heat 1 is Ace's, so the offer is heat 2.
    expect(screen.getByRole('button', { name: 'Pull Cat forward to heat 2' })).toBeVisible();
    // Ace is in the first heat: there is nowhere forward to go.
    expect(screen.queryByRole('button', { name: /Pull Ace/ })).toBeNull();
  });

  it('closes a heat to pilots and opens it again', async () => {
    const { send } = open(
      signupSheet({
        heats: [signupHeat(1, ['p-ace', null, null]), signupHeat(2, [null, null, null], 'Closed')]
      })
    );
    await fireEvent.click(
      screen.getByRole('button', { name: 'Close Qualifying Heat 1 to pilots' })
    );
    await fireEvent.click(screen.getByRole('button', { name: 'Open Qualifying Heat 2 to pilots' }));
    expect(sent(send)).toEqual([
      { Signup: { CloseHeat: { heat: 'quali/heat-1', closed: true } } },
      { Signup: { CloseHeat: { heat: 'quali/heat-2', closed: false } } }
    ]);
  });

  it('a heat that is on or flown can be changed by nobody', () => {
    open(
      signupSheet({
        heats: [
          signupHeat(1, ['p-ace', null, null], 'Flown'),
          signupHeat(2, ['p-bee', null, null], 'Racing')
        ]
      })
    );
    for (const row of [heatRow(1), heatRow(2)]) {
      expect(within(row).queryAllByRole('button')).toEqual([]);
    }
    expect(heatRow(1)).toHaveTextContent('Flown');
    expect(heatRow(2)).toHaveTextContent('On now');
  });

  it("lists each pilot's runs: flown, held and left", () => {
    open(
      signupSheet({
        rules: { ...SIGNUP_RULES, run_limit: 3 },
        pilots: [
          signupRuns('p-cat', { used: 2, held: 1, allowed: 3 }),
          signupRuns('p-ace', { used: 0, held: 1, allowed: 4, extra: 1 }),
          signupRuns('p-bee', { allowed: 2, extra: -1 })
        ]
      })
    );
    const rows = within(runs())
      .getAllByRole('row')
      .slice(1)
      .map((row) =>
        within(row)
          .getAllByRole('cell')
          .slice(0, 3)
          .map((cell) => cell.textContent?.trim())
          .concat(within(row).getByRole('rowheader').textContent ?? '')
      );
    // By callsign.
    expect(rows).toEqual([
      ['0', '1', '3', 'Ace'],
      ['0', '0', '2', 'Bee'],
      ['2', '1', '0', 'Cat']
    ]);
    const extras = within(runs())
      .getAllByTestId('extra')
      .map((el) => el.textContent?.trim());
    expect(extras).toEqual(['+1', '-1']);
  });

  it('gives one more run to everyone, or one more or fewer to one pilot', async () => {
    const { send } = open(
      signupSheet({
        rules: { ...SIGNUP_RULES, run_limit: 2 },
        pilots: [signupRuns('p-ace', { allowed: 3, extra: 1 }), signupRuns('p-bee', { allowed: 2 })]
      })
    );
    await fireEvent.click(screen.getByRole('button', { name: 'One more run for everyone' }));
    await fireEvent.click(screen.getByRole('button', { name: 'One more run for Ace' }));
    await fireEvent.click(screen.getByRole('button', { name: 'One run fewer for Bee' }));
    expect(sent(send)).toEqual([
      { Signup: { Set: { phase: 'quali', rules: { ...SIGNUP_RULES, run_limit: 3 } } } },
      { Signup: { SetRuns: { phase: 'quali', pilot: 'p-ace', extra: 2 } } },
      { Signup: { SetRuns: { phase: 'quali', pilot: 'p-bee', extra: -1 } } }
    ]);
  });

  it('with no run limit there is nothing to add to, and it says where to set one', () => {
    open();
    expect(screen.queryByRole('button', { name: 'One more run for everyone' })).toBeNull();
    expect(screen.queryByRole('button', { name: /One more run for Ace/ })).toBeNull();
    expect(screen.getByText('No limit on runs. Set one under Rules.')).toBeVisible();
    expect(within(runs()).getAllByRole('row')[1]).toHaveTextContent('—');
  });

  it("shows the Director's reason when a change is refused", async () => {
    const { send } = open();
    send.mockRejectedValueOnce(new Error('Qualifying is locked; unlock it first'));
    await fireEvent.click(screen.getByRole('button', { name: 'Close sign-up' }));
    expect(await screen.findByText('Qualifying is locked; unlock it first')).toBeVisible();
  });

  it('a locked phase shows the sheet and offers no change', () => {
    open(signupSheet({ rules: { ...SIGNUP_RULES, run_limit: 2 } }), true);
    expect(screen.getByText('This phase is locked, so the sheet cannot be changed.')).toBeVisible();
    expect(screen.queryAllByRole('button')).toEqual([]);
    expect(heatRow(1)).toHaveTextContent('Ace');
  });

  it('sends one command however fast a button is pressed', async () => {
    const { send } = open();
    let finish: () => void = () => {};
    send.mockImplementationOnce(() => new Promise<void>((resolve) => (finish = resolve)));
    const close = screen.getByRole('button', { name: 'Close sign-up' });
    await fireEvent.click(close);
    await fireEvent.click(close);
    await fireEvent.click(
      screen.getByRole('button', { name: 'Close Qualifying Heat 1 to pilots' })
    );
    expect(send).toHaveBeenCalledOnce();
    finish();
  });
});
