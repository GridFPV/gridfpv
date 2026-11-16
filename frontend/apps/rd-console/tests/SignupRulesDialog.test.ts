/**
 * The dialog that turns sign-up on for a phase and sets its rules.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { Command, SignupRules } from '@gridfpv/types';
import { seatSources } from '../src/lib/seats.js';
import SignupRulesDialog from '../src/screens/SignupRulesDialog.svelte';
import { timerView } from './competition.js';
import { SIGNUP_RULES } from './signupFixtures.js';

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const SOURCES = seatSources([timerView('gate', 'Gate', 3), timerView('spare', 'Spare', 0)]);

function open(rules?: SignupRules) {
  const send = vi.fn(async (_command: Command): Promise<unknown> => true);
  const onclose = vi.fn();
  render(SignupRulesDialog, {
    phase: 'quali',
    phaseName: 'Qualifying',
    rules,
    sources: SOURCES,
    send,
    onclose
  });
  return { send, onclose };
}

const box = (name: string) => screen.getByLabelText(name) as HTMLInputElement;
const form = () => screen.getByRole('form', { name: 'Sign-up rules' });
const type = (name: string, value: string) => fireEvent.input(box(name), { target: { value } });

describe('SignupRulesDialog', () => {
  it('starts a new sign-up on the first timer with every seat, three heats and no limits', () => {
    open();
    expect(screen.getByRole('heading')).toHaveTextContent('Pilot sign-up: Qualifying');
    expect((screen.getByLabelText('Timed by') as HTMLSelectElement).value).toBe('gate');
    for (const seat of ['Seat 1', 'Seat 2', 'Seat 3']) {
      expect(screen.getByRole('checkbox', { name: seat })).toBeChecked();
    }
    expect(box('Heats open at once').value).toBe('3');
    expect(box('Runs for each pilot').value).toBe('');
    expect(box('Spots held at once').value).toBe('');
    expect(box('Heats to sit out between two').value).toBe('0');
    expect(screen.getByRole('button', { name: 'Turn sign-up on' })).toBeEnabled();
    expect(screen.queryByRole('button', { name: 'Turn sign-up off' })).toBeNull();
  });

  it('turns sign-up on, closed to pilots, with the seats that are ticked', async () => {
    const { send, onclose } = open();
    await fireEvent.click(screen.getByRole('checkbox', { name: 'Seat 2' }));
    await type('Runs for each pilot', '4');
    await type('Spots held at once', '2');
    await fireEvent.submit(form());
    await waitFor(() => expect(onclose).toHaveBeenCalledOnce());
    expect(send).toHaveBeenCalledWith({
      Signup: {
        Set: {
          phase: 'quali',
          rules: {
            seats: [
              { adapter: 'gate', seat: '0' },
              { adapter: 'gate', seat: '2' }
            ],
            open_heats: 3,
            rest_heats: 0,
            open: false,
            run_limit: 4,
            held_limit: 2
          }
        }
      }
    });
  });

  it('a timer with no seats cannot be picked, and says why', () => {
    open();
    const spare = within(screen.getByLabelText('Timed by')).getByRole('option', { name: /Spare/ });
    expect(spare).toBeDisabled();
    expect(spare.textContent).toMatch(/\(.+\)/);
  });

  it('picking another source starts again from all of its seats', async () => {
    open();
    await fireEvent.click(screen.getByRole('checkbox', { name: 'Seat 1' }));
    await fireEvent.change(screen.getByLabelText('Timed by'), { target: { value: 'bench' } });
    const boxes = screen.getAllByRole('checkbox');
    expect(boxes.length).toBeGreaterThan(3);
    for (const seat of boxes) expect(seat).toBeChecked();
  });

  it('says what is wrong and sends nothing', async () => {
    const { send } = open();
    for (const [name, value, words] of [
      ['Heats open at once', '0', 'Heats open at once: a number from 1 to 20.'],
      ['Heats open at once', '3', undefined],
      ['Runs for each pilot', '0', 'Runs for each pilot: a number from 1 up'],
      ['Runs for each pilot', '', undefined],
      ['Spots held at once', 'two', 'Spots held at once: a number from 1 up'],
      ['Spots held at once', '', undefined],
      ['Heats to sit out between two', '', 'Heats to sit out: a number from 0 to 20.']
    ] as const) {
      await type(name, value);
      if (words === undefined) continue;
      await fireEvent.submit(form());
      expect(screen.getByRole('alert'), `${name}=${value}`).toHaveTextContent(words);
    }
    expect(send).not.toHaveBeenCalled();
  });

  it('needs at least one seat', async () => {
    const { send } = open();
    for (const seat of ['Seat 1', 'Seat 2', 'Seat 3']) {
      await fireEvent.click(screen.getByRole('checkbox', { name: seat }));
    }
    await fireEvent.submit(form());
    expect(screen.getByRole('alert')).toHaveTextContent('Pick at least one seat.');
    expect(send).not.toHaveBeenCalled();
  });

  it('does not nag before the director has tried to save', async () => {
    open();
    await type('Heats open at once', '');
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('shows the rules in force and saves a change, keeping whether it is open', async () => {
    const { send } = open({ ...SIGNUP_RULES, run_limit: 2, held_limit: 1, rest_heats: 1 });
    expect(box('Runs for each pilot').value).toBe('2');
    expect(box('Spots held at once').value).toBe('1');
    expect(box('Heats to sit out between two').value).toBe('1');
    expect(screen.getByRole('button', { name: 'Save' })).toBeEnabled();
    await type('Runs for each pilot', '3');
    await type('Spots held at once', '');
    await fireEvent.submit(form());
    await waitFor(() => expect(send).toHaveBeenCalledOnce());
    expect(send.mock.calls[0][0]).toEqual({
      Signup: {
        Set: { phase: 'quali', rules: { ...SIGNUP_RULES, run_limit: 3, rest_heats: 1 } }
      }
    });
  });

  it('turns sign-up off, which sends no rules', async () => {
    const { send, onclose } = open(SIGNUP_RULES);
    await fireEvent.click(screen.getByRole('button', { name: 'Turn sign-up off' }));
    await waitFor(() => expect(onclose).toHaveBeenCalledOnce());
    expect(send).toHaveBeenCalledWith({ Signup: { Set: { phase: 'quali' } } });
  });

  it("stays open and shows the Director's reason when it refuses", async () => {
    const { send, onclose } = open();
    send.mockRejectedValueOnce(new Error('Qualifying is locked; unlock it first'));
    await fireEvent.submit(form());
    expect(await screen.findByText('Qualifying is locked; unlock it first')).toBeVisible();
    expect(onclose).not.toHaveBeenCalled();
  });

  it('cancel closes it and sends nothing', async () => {
    const { send, onclose } = open();
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(onclose).toHaveBeenCalledOnce();
    expect(send).not.toHaveBeenCalled();
  });
});
