/**
 * The kiosk: a tablet the director sets up, where a pilot types their code, signs up,
 * and walks away.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { SignupView } from '@gridfpv/types';
import KioskPage from '../src/screens/KioskPage.svelte';
import { signupEvent, signupNames, signupSheet } from './signupFixtures.js';

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const BEE = { pilot: { id: 'p-bee', callsign: 'Bee' }, pass: 'pass-bee-1', idle_seconds: 120 };
const refused = (status: number, message: string) => Object.assign(new Error(message), { status });

function open(
  over: {
    isKiosk?: boolean;
    eventName?: string | undefined;
    sheet?: SignupView | 'none';
    offline?: boolean;
  } = {}
) {
  const view = signupEvent(over.sheet === 'none' || !over.sheet ? signupSheet() : over.sheet);
  if (over.sheet === 'none') {
    view.phases = view.phases.map((phase) => ({ ...phase, signup: undefined }));
  }
  const source = {
    kioskSetUp: vi.fn(async (_code: string) => {}),
    kioskIn: vi.fn(async (_code: string) => BEE),
    kioskDone: vi.fn(async (_pass: string) => {}),
    takeSpot: vi.fn(async (..._args: unknown[]): Promise<unknown> => undefined),
    takeNextSpot: vi.fn(async (..._args: unknown[]): Promise<unknown> => undefined),
    leaveSpot: vi.fn(async (..._args: unknown[]): Promise<unknown> => undefined),
    takeSeat: vi.fn(async (..._args: unknown[]): Promise<unknown> => undefined),
    dropSeat: vi.fn(async (..._args: unknown[]): Promise<unknown> => undefined)
  };
  const live = { view, names: signupNames(view), reload: vi.fn(async () => {}) };
  const props = {
    source,
    isKiosk: over.isKiosk ?? true,
    eventName: 'eventName' in over ? over.eventName : 'Friday Night Whoops',
    live: 'eventName' in over && over.eventName === undefined ? undefined : live,
    offline: over.offline ?? false
  };
  const page = render(KioskPage, props);
  return { source, live, page, props };
}

const setupBox = () => screen.getByLabelText('Kiosk code') as HTMLInputElement;
const codeBox = () => screen.getByLabelText('Type your code') as HTMLInputElement;
async function setUp(typed: string) {
  await fireEvent.input(setupBox(), { target: { value: typed } });
  await fireEvent.submit(screen.getByRole('form', { name: 'Set up kiosk' }));
}
async function typeCode(typed: string) {
  await fireEvent.input(codeBox(), { target: { value: typed } });
  await fireEvent.submit(screen.getByRole('form', { name: 'Pilot code' }));
}
async function beeSignsIn() {
  await typeCode('ABC 234');
  await screen.findByTestId('kiosk-pilot');
}
const heatRow = (n: number) => screen.getByRole('row', { name: `Qualifying Heat ${n}` });

describe('KioskPage: setting the tablet up', () => {
  it('a tablet the director has not set up asks for a kiosk code and offers nothing else', () => {
    open({ isKiosk: false });
    expect(setupBox()).toBeVisible();
    expect(screen.getByText(/For the race director\./)).toBeVisible();
    expect(screen.queryByLabelText('Type your code')).toBeNull();
    expect(screen.queryByRole('table')).toBeNull();
    expect(screen.getAllByRole('button').map((b) => b.textContent?.trim())).toEqual(['Set up']);
  });

  it('a right kiosk code, tidied, makes it a kiosk and asks for a pilot code', async () => {
    const { source } = open({ isKiosk: false });
    await setUp(' abcd-2345 ');
    await waitFor(() => expect(source.kioskSetUp).toHaveBeenCalledWith('ABCD2345'));
    expect(await screen.findByLabelText('Type your code')).toBeVisible();
    expect(screen.queryByLabelText('Kiosk code')).toBeNull();
  });

  it('catches a slip before it counts as a wrong try', async () => {
    const { source } = open({ isKiosk: false });
    for (const [typed, words] of [
      ['', 'Type the kiosk code.'],
      ['ABCD234', 'A kiosk code has 8 letters and numbers.'],
      ['ABCD23456', 'A kiosk code has 8 letters and numbers.'],
      ['ABCD10OI', 'A kiosk code has capital letters and numbers, and never 0, O, 1 or I.']
    ] as const) {
      await setUp(typed);
      expect(screen.getByRole('alert'), typed).toHaveTextContent(words);
    }
    expect(source.kioskSetUp).not.toHaveBeenCalled();
  });

  it("shows the Director's reason for a wrong or used code, and clears the box", async () => {
    const { source } = open({ isKiosk: false });
    source.kioskSetUp.mockRejectedValueOnce(
      refused(401, 'That kiosk code is not right, or it has been used.')
    );
    await setUp('ZZZZ ZZZZ');
    expect(await screen.findByRole('alert')).toHaveTextContent('or it has been used');
    expect(setupBox().value).toBe('');
    expect(screen.queryByLabelText('Type your code')).toBeNull();
    // The next one works, and the old reason is gone.
    await setUp('ABCD 2345');
    expect(await screen.findByLabelText('Type your code')).toBeVisible();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('sends one request however fast Set up is pressed', async () => {
    const { source } = open({ isKiosk: false });
    let finish: () => void = () => {};
    source.kioskSetUp.mockImplementationOnce(() => new Promise<void>((done) => (finish = done)));
    await fireEvent.input(setupBox(), { target: { value: 'ABCD2345' } });
    const form = screen.getByRole('form', { name: 'Set up kiosk' });
    await fireEvent.submit(form);
    await fireEvent.submit(form);
    expect(source.kioskSetUp).toHaveBeenCalledOnce();
    finish();
    await screen.findByLabelText('Type your code');
  });

  it('goes back to setting up when the director has removed this kiosk', async () => {
    const { source } = open();
    source.kioskIn.mockRejectedValueOnce(refused(403, 'This device is not set up as a kiosk.'));
    await typeCode('ABC234');
    expect(await screen.findByLabelText('Kiosk code')).toBeVisible();
    expect(screen.queryByLabelText('Type your code')).toBeNull();
  });
});

describe('KioskPage: waiting for a pilot', () => {
  it('shows the event and a code box, and nothing of the race or the sheet', () => {
    open();
    expect(screen.getByTestId('visitor-event')).toHaveTextContent('Friday Night Whoops');
    expect(codeBox()).toBeVisible();
    expect(screen.queryByRole('table')).toBeNull();
    expect(screen.queryByRole('tablist')).toBeNull();
    expect(document.body.textContent).not.toContain('Ace');
    expect(screen.getAllByRole('button').map((b) => b.textContent?.trim())).toEqual(['Go']);
  });

  it('shows what the director calls it while it waits, and not while a pilot is at it', async () => {
    const { page, props } = open();
    expect(screen.queryByTestId('kiosk-name')).toBeNull();
    await page.rerender({ ...props, kioskName: '<b>By the gate</b>' });
    // As text, whatever is in it.
    expect(screen.getByTestId('kiosk-name').textContent).toBe('<b>By the gate</b>');
    await beeSignsIn();
    expect(screen.queryByTestId('kiosk-name')).toBeNull();
  });

  it('a tablet that is not set up shows no kiosk name', () => {
    render(KioskPage, {
      source: {
        kioskSetUp: vi.fn(),
        kioskIn: vi.fn(),
        kioskDone: vi.fn(),
        takeSpot: vi.fn(),
        takeNextSpot: vi.fn(),
        leaveSpot: vi.fn(),
        takeSeat: vi.fn(async (..._args: unknown[]): Promise<unknown> => undefined),
        dropSeat: vi.fn(async (..._args: unknown[]): Promise<unknown> => undefined)
      },
      isKiosk: false,
      kioskName: 'Leftover',
      eventName: 'Friday Night Whoops',
      live: undefined
    });
    expect(screen.queryByTestId('kiosk-name')).toBeNull();
  });

  it('with no event open it says so and has no code box', () => {
    open({ eventName: undefined });
    expect(screen.getByTestId('visitor-event')).toHaveTextContent('No event is open');
    expect(screen.queryByLabelText('Type your code')).toBeNull();
  });

  it('says when Grid cannot be reached', () => {
    open({ offline: true });
    expect(screen.getByTestId('visitor-offline')).toBeVisible();
  });

  it('a right code greets the pilot and shows the sheet as a grid', async () => {
    const { source } = open();
    await typeCode('abc-234');
    expect(await screen.findByTestId('kiosk-pilot')).toHaveTextContent('Hi Bee');
    expect(source.kioskIn).toHaveBeenCalledWith('ABC234');
    expect(screen.queryByLabelText('Type your code')).toBeNull();
    const sheet = screen.getByRole('table', { name: 'Heats of Qualifying' });
    expect(
      within(sheet)
        .getAllByRole('columnheader')
        .map((cell) => cell.textContent?.replace(/\s+/g, ' ').trim())
    ).toEqual(['Heat', 'Seat 1 Next free', 'Seat 2 Next free', 'Seat 3 Next free']);
    expect(heatRow(1)).toHaveTextContent('Ace');
    expect(screen.getAllByRole('button', { name: /^Take/ }).length).toBeGreaterThan(0);
    // No raw id anywhere.
    expect(document.body.textContent).not.toContain('p-ace');
    expect(document.body.textContent).not.toContain('quali/heat');
  });

  it('takes the code out of a whole link, as a handheld scanner types it', async () => {
    const { source } = open();
    await typeCode('http://192.168.1.20:8081/#/join/abc234');
    await screen.findByTestId('kiosk-pilot');
    expect(source.kioskIn).toHaveBeenCalledWith('ABC234');
  });

  it('catches a slip before it counts as a wrong try', async () => {
    const { source } = open();
    for (const [typed, words] of [
      ['', 'Type your code.'],
      ['ABC', 'A code has 6 letters and numbers.'],
      ['ABC2345', 'A code has only 6 letters and numbers.'],
      ['ABC10O', 'A code has capital letters and numbers, and never 0, O, 1 or I.'],
      ['http://x/#/join/', 'Type your code.']
    ] as const) {
      await typeCode(typed);
      expect(screen.getByRole('alert'), typed).toHaveTextContent(words);
    }
    expect(source.kioskIn).not.toHaveBeenCalled();
  });

  it("shows the Director's reason for a wrong code, and does not leave the code on the screen", async () => {
    const { source } = open();
    source.kioskIn.mockRejectedValueOnce(
      refused(401, 'That code is not right. Check it with the director.')
    );
    await typeCode('ZZZ ZZZ');
    expect(await screen.findByRole('alert')).toHaveTextContent('That code is not right.');
    expect(codeBox().value).toBe('');
    expect(screen.queryByTestId('kiosk-pilot')).toBeNull();
    await beeSignsIn();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('one press is one request, however fast the code is sent', async () => {
    const { source } = open();
    let finish: (value: typeof BEE) => void = () => {};
    source.kioskIn.mockImplementationOnce(() => new Promise((resolve) => (finish = resolve)));
    await fireEvent.input(codeBox(), { target: { value: 'ABC234' } });
    const form = screen.getByRole('form', { name: 'Pilot code' });
    await fireEvent.submit(form);
    await fireEvent.submit(form);
    expect(source.kioskIn).toHaveBeenCalledOnce();
    finish(BEE);
    await screen.findByTestId('kiosk-pilot');
  });
});

describe('KioskPage: a pilot signing up', () => {
  it("takes a seat with the pilot's pass, and reads the event again", async () => {
    const { source, live } = open();
    await beeSignsIn();
    await fireEvent.click(screen.getByRole('button', { name: 'Take Seat 2 in Qualifying Heat 1' }));
    await waitFor(() =>
      expect(source.takeSpot).toHaveBeenCalledWith(
        'quali/heat-1',
        { adapter: 'gate', seat: '1' },
        'pass-bee-1'
      )
    );
    await waitFor(() => expect(live.reload).toHaveBeenCalledOnce());
  });

  it('takes the next free spot on a seat from the column, with the pass', async () => {
    const { source } = open();
    await beeSignsIn();
    await fireEvent.click(screen.getByRole('button', { name: 'Next free spot on Seat 3' }));
    await waitFor(() =>
      expect(source.takeNextSpot).toHaveBeenCalledWith(
        'quali',
        { adapter: 'gate', seat: '2' },
        'pass-bee-1'
      )
    );
    // Each seat has the button once: the grid does not repeat the phone's row of them.
    expect(screen.getAllByRole('button', { name: 'Next free spot on Seat 3' })).toHaveLength(1);
  });

  it("leaves their own seat, and cannot leave anyone else's", async () => {
    const { source } = open();
    source.kioskIn.mockResolvedValueOnce({
      pilot: { id: 'p-ace', callsign: 'Ace' },
      pass: 'pass-ace',
      idle_seconds: 120
    });
    await typeCode('ABC234');
    await screen.findByTestId('kiosk-pilot');
    expect(screen.getAllByRole('button', { name: /^Leave/ })).toHaveLength(1);
    await fireEvent.click(screen.getByRole('button', { name: 'Leave Qualifying Heat 1' }));
    await waitFor(() => expect(source.leaveSpot).toHaveBeenCalledWith('quali/heat-1', 'pass-ace'));
  });

  it('says why a seat cannot be taken', async () => {
    open({
      sheet: signupSheet({
        rules: { ...signupSheet().rules, open: false }
      })
    });
    await beeSignsIn();
    expect(heatRow(2)).toHaveTextContent('Sign-up is closed');
    expect(screen.queryByRole('button', { name: /^Take/ })).toBeNull();
  });

  it('says so when the event has nothing to sign up for', async () => {
    open({ sheet: 'none' });
    await beeSignsIn();
    expect(screen.getByText('There is nothing to sign up for right now.')).toBeVisible();
    expect(screen.getByRole('button', { name: 'Done' })).toBeVisible();
  });

  it('Done signs the pilot out, tells Grid, and asks for the next code', async () => {
    const { source } = open();
    await beeSignsIn();
    await fireEvent.click(screen.getByRole('button', { name: 'Done' }));
    expect(screen.queryByTestId('kiosk-pilot')).toBeNull();
    expect(source.kioskDone).toHaveBeenCalledWith('pass-bee-1');
    expect(codeBox()).toBeVisible();
    expect(screen.queryByRole('table')).toBeNull();
    // Nothing is said: the pilot asked for it.
    expect(screen.queryByTestId('kiosk-note')).toBeNull();
  });

  it('Done still signs the pilot out when Grid cannot be told', async () => {
    const { source } = open();
    source.kioskDone.mockRejectedValue(new Error('offline'));
    await beeSignsIn();
    await fireEvent.click(screen.getByRole('button', { name: 'Done' }));
    expect(screen.queryByTestId('kiosk-pilot')).toBeNull();
    expect(screen.queryByRole('table')).toBeNull();
  });

  it('signs the pilot out when nobody touches the screen, counting down first', async () => {
    const { source } = open();
    await beeSignsIn();
    expect(screen.queryByTestId('kiosk-warning')).toBeNull();
    await vi.advanceTimersByTimeAsync(29_000);
    expect(screen.queryByTestId('kiosk-warning')).toBeNull();
    await vi.advanceTimersByTimeAsync(2_000);
    expect(screen.getByTestId('kiosk-warning')).toHaveTextContent(
      /^Signing out in 1[45] s\. Touch the screen to stay\.$/
    );
    await vi.advanceTimersByTimeAsync(15_000);
    expect(screen.queryByTestId('kiosk-pilot')).toBeNull();
    expect(screen.getByTestId('kiosk-note')).toHaveTextContent(
      'You were signed out. Nobody touched the screen.'
    );
    expect(source.kioskDone).toHaveBeenCalledWith('pass-bee-1');
    expect(screen.queryByRole('table')).toBeNull();
  });

  it('a touch or a key keeps the pilot signed in', async () => {
    const { source } = open();
    await beeSignsIn();
    for (const event of ['pointerdown', 'keydown', 'wheel', 'touchstart']) {
      await vi.advanceTimersByTimeAsync(40_000);
      expect(screen.getByTestId('kiosk-warning')).toBeVisible();
      window.dispatchEvent(new Event(event));
      await vi.advanceTimersByTimeAsync(600);
      expect(screen.queryByTestId('kiosk-warning'), event).toBeNull();
    }
    expect(screen.getByTestId('kiosk-pilot')).toBeVisible();
    expect(source.kioskDone).not.toHaveBeenCalled();
  });

  it('the next pilot gets a full count, not what the last one left, and their own pass', async () => {
    const { source } = open();
    await beeSignsIn();
    await vi.advanceTimersByTimeAsync(44_000);
    await fireEvent.click(screen.getByRole('button', { name: 'Done' }));
    source.kioskIn.mockResolvedValueOnce({
      pilot: { id: 'p-cat', callsign: 'Cat' },
      pass: 'pass-cat',
      idle_seconds: 120
    });
    await typeCode('DEF 567');
    expect(await screen.findByTestId('kiosk-pilot')).toHaveTextContent('Hi Cat');
    await vi.advanceTimersByTimeAsync(20_000);
    expect(screen.getByTestId('kiosk-pilot')).toHaveTextContent('Hi Cat');
    expect(screen.queryByTestId('kiosk-warning')).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Take Seat 2 in Qualifying Heat 1' }));
    await waitFor(() =>
      expect(source.takeSpot).toHaveBeenCalledWith(expect.anything(), expect.anything(), 'pass-cat')
    );
  });

  it('signs the pilot out when Grid no longer knows the pass', async () => {
    const { source, live } = open();
    await beeSignsIn();
    source.takeSpot.mockRejectedValueOnce(refused(401, 'Sign in with your code first.'));
    await fireEvent.click(screen.getByRole('button', { name: 'Take Seat 2 in Qualifying Heat 1' }));
    expect(await screen.findByTestId('kiosk-note')).toHaveTextContent(
      'You were signed out. Type your code again.'
    );
    expect(screen.queryByTestId('kiosk-pilot')).toBeNull();
    expect(screen.queryByRole('table')).toBeNull();
    expect(live.reload).not.toHaveBeenCalled();
  });

  it("shows the Director's reason when the rules refuse, and keeps the pilot signed in", async () => {
    const { source } = open();
    await beeSignsIn();
    source.takeSpot.mockRejectedValueOnce(
      refused(409, 'that seat in Qualifying Heat 1 is already taken')
    );
    await fireEvent.click(screen.getByRole('button', { name: 'Take Seat 2 in Qualifying Heat 1' }));
    expect(await screen.findByTestId('signup-error')).toHaveTextContent('already taken');
    expect(screen.getByTestId('kiosk-pilot')).toHaveTextContent('Hi Bee');
  });

  it('signs the pilot out when the event closes', async () => {
    const { source, page, props } = open();
    await beeSignsIn();
    await page.rerender({ ...props, eventName: undefined, live: undefined });
    await waitFor(() => expect(source.kioskDone).toHaveBeenCalledWith('pass-bee-1'));
    expect(screen.queryByTestId('kiosk-pilot')).toBeNull();
    expect(screen.getByTestId('visitor-event')).toHaveTextContent('No event is open');
  });
});

describe('KioskPage: one sheet at a time', () => {
  /** An event with two sign-up phases. Bee is in both, Cat only in Qualifying. */
  function openTwo() {
    const first = signupSheet();
    const view = signupEvent(first);
    const sport: SignupView = {
      ...signupSheet(),
      heats: [
        { heat: 'sport/heat-1', number: 1, seats: [null, null, null], state: 'Open', full: false }
      ],
      pilots: [{ pilot: 'p-bee', used: 0, held: 0, extra: 0 }]
    };
    view.phases = [
      ...view.phases,
      {
        ...view.phases[0],
        def: { ...view.phases[0].def, id: 'sport', label: 'Sport' },
        signup: sport
      },
      {
        ...view.phases[1],
        def: { ...view.phases[1].def, id: 'sport/heat-1', label: 'Sport Heat 1', parent: 'sport' },
        lineup: []
      }
    ];
    const source = {
      kioskSetUp: vi.fn(async (_code: string) => {}),
      kioskIn: vi.fn(async (_code: string) => BEE),
      kioskDone: vi.fn(async (_pass: string) => {}),
      takeSpot: vi.fn(async (..._args: unknown[]): Promise<unknown> => undefined),
      takeNextSpot: vi.fn(async (..._args: unknown[]): Promise<unknown> => undefined),
      leaveSpot: vi.fn(async (..._args: unknown[]): Promise<unknown> => undefined),
      takeSeat: vi.fn(async (..._args: unknown[]): Promise<unknown> => undefined),
      dropSeat: vi.fn(async (..._args: unknown[]): Promise<unknown> => undefined)
    };
    const live = { view, names: signupNames(view), reload: vi.fn(async () => {}) };
    render(KioskPage, { source, isKiosk: true, eventName: 'Friday Night Whoops', live });
    return { source };
  }
  const picker = () => screen.getByRole('combobox', { name: 'Sign up for' }) as HTMLSelectElement;
  const tables = () => screen.getAllByRole('table').map((el) => el.getAttribute('aria-label'));

  it('with one sheet there is no picker', async () => {
    open();
    await beeSignsIn();
    expect(screen.queryByRole('combobox')).toBeNull();
    expect(tables()).toEqual(['Heats of Qualifying']);
  });

  it('with several, shows the first and a picker for the rest', async () => {
    openTwo();
    await beeSignsIn();
    expect([...picker().options].map((o) => o.textContent)).toEqual(['Qualifying', 'Sport']);
    expect(picker().selectedOptions[0].textContent).toBe('Qualifying');
    expect(tables()).toEqual(['Heats of Qualifying']);
    await fireEvent.change(picker(), { target: { value: 'sport' } });
    expect(tables()).toEqual(['Heats of Sport']);
    expect(document.body.textContent).not.toContain('sport/heat');
  });

  it('acts on the sheet that is showing', async () => {
    const { source } = openTwo();
    await beeSignsIn();
    await fireEvent.change(picker(), { target: { value: 'sport' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Take Seat 1 in Sport Heat 1' }));
    await waitFor(() =>
      expect(source.takeSpot).toHaveBeenCalledWith(
        'sport/heat-1',
        { adapter: 'gate', seat: '0' },
        'pass-bee-1'
      )
    );
  });

  it('lists only the sheets the pilot is in, and no picker when that leaves one', async () => {
    const { source } = openTwo();
    source.kioskIn.mockResolvedValueOnce({
      pilot: { id: 'p-cat', callsign: 'Cat' },
      pass: 'pass-cat',
      idle_seconds: 120
    });
    await typeCode('ABC234');
    await screen.findByTestId('kiosk-pilot');
    expect(screen.queryByRole('combobox')).toBeNull();
    expect(tables()).toEqual(['Heats of Qualifying']);
    expect(document.body.textContent).not.toContain('You are not in this phase');
  });

  it('a pilot who is in none is told there is nothing to sign up for', async () => {
    const { source } = openTwo();
    source.kioskIn.mockResolvedValueOnce({
      pilot: { id: 'p-stranger', callsign: 'Stranger' },
      pass: 'pass-s',
      idle_seconds: 120
    });
    await typeCode('ABC234');
    await screen.findByTestId('kiosk-pilot');
    expect(screen.queryByRole('table')).toBeNull();
    expect(screen.getByText('There is nothing to sign up for right now.')).toBeVisible();
  });

  it('the next pilot starts on the first sheet, not where the last one left off', async () => {
    openTwo();
    await beeSignsIn();
    await fireEvent.change(picker(), { target: { value: 'sport' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Done' }));
    await beeSignsIn();
    expect(picker().selectedOptions[0].textContent).toBe('Qualifying');
    expect(tables()).toEqual(['Heats of Qualifying']);
  });
});
