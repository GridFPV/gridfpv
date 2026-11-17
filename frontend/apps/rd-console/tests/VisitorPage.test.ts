/**
 * The page someone on another device sees, and the director's sign-in on it.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { PublicEvent, PublicHello } from '@gridfpv/types';
import type { PublicSource } from '../src/lib/spectator.js';
import VisitorPage from '../src/screens/VisitorPage.svelte';
import { eventView, heat, race, raceSeat } from './competition.js';
import { signupEvent, signupSheet, SIGNUP_PILOTS, GATE } from './signupFixtures.js';
import { fakeLiveSession } from './support.js';

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
});
afterEach(() => {
  cleanup();
  location.hash = '';
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const HELLO: PublicHello = {
  version: '0.5.1',
  caller: 'Visitor',
  event: 'Friday Night Whoops',
  director_sign_in: true,
  kiosk: false
};

/** What Grid answers when Bee types their code at a kiosk. */
const KIOSK_BEE = {
  pilot: { id: 'p-bee', callsign: 'Bee' },
  pass: 'pass-bee-1',
  idle_seconds: 120
};

const NIGHT: PublicEvent = { event: { id: 'evt-night', name: 'Friday Night Whoops' } };

/** A running race in the open event, for the pages that show it. */
function racing() {
  const on = race('h1', 1, 'Running', {
    started_at: 100_000_000,
    seats: [raceSeat(0, 'p-ace', { finish: 1 })]
  });
  return eventView({
    now: 105_000_000,
    phases: [heat('h1', 'Qualifying Heat 1', ['p-ace'])],
    races: [on],
    current_race: on.race
  });
}

function open(hello: PublicHello = HELLO, event: PublicEvent = hello.event ? NIGHT : {}) {
  const session = {
    signInDirector: vi.fn(async (_token: string) => {}),
    signInPilot: vi.fn(async (_code: string) => ({ id: 'p-ace', callsign: 'Ace' })),
    signOutPilot: vi.fn(async () => {})
  };
  const reload = vi.fn();
  const stop = vi.fn();
  const source: PublicSource = {
    ...fakeLiveSession(racing(), {
      listPilots: vi.fn(async () => [{ id: 'p-ace', callsign: 'Ace', vtx_types: [] }]),
      subscribeChanges: vi.fn(() => stop)
    }),
    getEvent: vi.fn(async () => event),
    getChannels: vi.fn(async () => [{ adapter: 'bench', seat: 'seat-1', freq_mhz: 5658 }]),
    takeSpot: vi.fn(async () => {}),
    takeNextSpot: vi.fn(async () => {}),
    leaveSpot: vi.fn(async () => {}),
    kioskIn: vi.fn(async () => KIOSK_BEE),
    kioskDone: vi.fn(async () => {}),
    kioskSetUp: vi.fn(async () => {})
  };
  render(VisitorPage, { session, source, hello, reload });
  return {
    session,
    reload,
    source,
    stop,
    /** What the Director says is open from now on. */
    becomes: (next: PublicEvent) => vi.mocked(source.getEvent).mockResolvedValue(next)
  };
}

/** Go to the address a director signs in at. Nothing on the page links to it. */
const askToSignIn = async () => {
  location.hash = '#/director';
  window.dispatchEvent(new Event('hashchange'));
  await screen.findByText('Director sign-in');
};
const tokenBox = () => screen.getByLabelText('Access token') as HTMLInputElement;

describe('VisitorPage', () => {
  it('names the open event, with the Pilot link in the top bar and no way to the director', () => {
    open();
    expect(screen.getByTestId('visitor-event')).toHaveTextContent('Friday Night Whoops');
    expect(within(screen.getByRole('banner')).getByRole('button', { name: 'Pilot' })).toBeVisible();
    // Nothing on the page points at the director's sign-in.
    expect(screen.queryByRole('button', { name: /director/i })).toBeNull();
    expect(screen.queryByRole('link', { name: /director/i })).toBeNull();
    expect(document.body.textContent).not.toMatch(/director/i);
  });

  it('says so when no event is open', () => {
    const { event: _none, ...rest } = HELLO;
    open(rest);
    expect(screen.getByTestId('visitor-event')).toHaveTextContent('No event is open');
  });

  it('shows an event name as text, whatever is in it', () => {
    open({ ...HELLO, event: '<img src=x onerror=alert(1)> & "Finals"' });
    expect(screen.getByTestId('visitor-event').textContent).toBe(
      '<img src=x onerror=alert(1)> & "Finals"'
    );
    expect(document.querySelector('img')).toBeNull();
  });

  it('has no director sign-in when Grid has no access token set, and says so at its address', async () => {
    open({ ...HELLO, director_sign_in: false });
    expect(screen.queryByLabelText('Access token')).toBeNull();
    await askToSignIn();
    expect(screen.queryByLabelText('Access token')).toBeNull();
    expect(screen.getByTestId('no-director-sign-in')).toHaveTextContent(
      'A director cannot sign in from another device.'
    );
  });

  it("the director's page shows the sign-in and nothing of the race or the pilots", async () => {
    open();
    await askToSignIn();
    expect(screen.queryByRole('button', { name: 'Pilot' })).toBeNull();
    expect(screen.queryByRole('tablist')).toBeNull();
    expect(screen.queryByTestId('visitor-event')).toBeNull();
  });

  it('opens straight on the sign-in when the page is loaded at its address', () => {
    location.hash = '#/director';
    open();
    expect(tokenBox()).toBeVisible();
  });

  it('only that exact address is the sign-in', () => {
    for (const hash of ['#/director/', '#/directors', '#director', '#/Director']) {
      location.hash = hash;
      open();
      expect(screen.queryByLabelText('Access token'), hash).toBeNull();
      cleanup();
    }
  });

  it('keeps the token box away until its address is opened, and hides what is typed', async () => {
    open();
    expect(screen.queryByLabelText('Access token')).toBeNull();
    await askToSignIn();
    expect(tokenBox()).toHaveAttribute('type', 'password');
    expect(tokenBox()).toHaveAttribute('autocomplete', 'off');
    expect(screen.getByRole('button', { name: 'Sign in' })).toBeDisabled();
  });

  it('signs in with the token and loads the page again', async () => {
    const { session, reload } = open();
    await askToSignIn();
    await fireEvent.input(tokenBox(), { target: { value: '  the-token  ' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    await waitFor(() => expect(reload).toHaveBeenCalledOnce());
    // The address is put back, so the console that loads next starts at its front page.
    expect(location.hash).toBe('');
    // Spaces around a pasted token are not part of it.
    expect(session.signInDirector).toHaveBeenCalledWith('the-token');
  });

  it("shows the Director's words for a wrong token and stays put", async () => {
    const { session, reload } = open();
    session.signInDirector.mockRejectedValueOnce(
      new Error("That is not the director's access token.")
    );
    await askToSignIn();
    await fireEvent.input(tokenBox(), { target: { value: 'nope' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(
      "That is not the director's access token."
    );
    expect(reload).not.toHaveBeenCalled();
    // A second try works, and the old message is gone while it is sent.
    await fireEvent.input(tokenBox(), { target: { value: 'the-token' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    await waitFor(() => expect(reload).toHaveBeenCalledOnce());
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('says so when a device has guessed too often', async () => {
    const { session } = open();
    session.signInDirector.mockRejectedValueOnce(
      new Error('Too many wrong access tokens from this device. Wait a minute and try again.')
    );
    await askToSignIn();
    await fireEvent.input(tokenBox(), { target: { value: 'guess' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Wait a minute and try again.');
  });

  it('sends nothing for an empty token, a token of spaces, or Enter on either', async () => {
    const { session } = open();
    await askToSignIn();
    const form = screen.getByRole('form', { name: 'Director sign-in' });
    await fireEvent.submit(form);
    await fireEvent.input(tokenBox(), { target: { value: '   ' } });
    expect(screen.getByRole('button', { name: 'Sign in' })).toBeDisabled();
    await fireEvent.submit(form);
    expect(session.signInDirector).not.toHaveBeenCalled();
  });

  it('sends a token once however fast the button is pressed', async () => {
    const { session } = open();
    let finish: () => void = () => {};
    session.signInDirector.mockImplementationOnce(
      () => new Promise<void>((resolve) => (finish = resolve))
    );
    await askToSignIn();
    await fireEvent.input(tokenBox(), { target: { value: 'the-token' } });
    const form = screen.getByRole('form', { name: 'Director sign-in' });
    await fireEvent.submit(form);
    await fireEvent.submit(form);
    await fireEvent.submit(form);
    expect(session.signInDirector).toHaveBeenCalledOnce();
    finish();
  });

  it('leaving the address puts the sign-in away', async () => {
    open();
    await askToSignIn();
    location.hash = '';
    window.dispatchEvent(new Event('hashchange'));
    await waitFor(() => expect(screen.queryByLabelText('Access token')).toBeNull());
    expect(screen.getByTestId('visitor-event')).toHaveTextContent('Friday Night Whoops');
  });
});

describe('VisitorPage: following the open event', () => {
  it('shows the open event to watch, read from the public routes', async () => {
    const { source } = open();
    expect(await screen.findByTestId('race-heat')).toHaveTextContent('Qualifying Heat 1');
    expect(source.getEventState).toHaveBeenCalledWith('evt-night');
    expect(source.subscribeChanges).toHaveBeenCalledOnce();
    expect(await screen.findByRole('rowheader', { name: /Ace/ })).toBeVisible();
  });

  it('names the event at once from the first answer, before the event has been read', () => {
    open();
    expect(screen.getByTestId('visitor-event')).toHaveTextContent('Friday Night Whoops');
  });

  it('follows the director to another event, and starts reading it afresh', async () => {
    const { source, stop, becomes } = open();
    await screen.findByTestId('race-heat');
    becomes({ event: { id: 'evt-sunday', name: 'Sunday Finals' } });
    await vi.advanceTimersByTimeAsync(5100);
    await waitFor(() =>
      expect(screen.getByTestId('visitor-event')).toHaveTextContent('Sunday Finals')
    );
    // The old event's feed is closed and the new one is opened.
    expect(stop).toHaveBeenCalledOnce();
    await waitFor(() => expect(source.getEventState).toHaveBeenCalledWith('evt-sunday'));
    expect(source.subscribeChanges).toHaveBeenCalledTimes(2);
  });

  it('takes a new name for the same event without reading it all again', async () => {
    const { source, stop, becomes } = open();
    await screen.findByTestId('race-heat');
    becomes({ event: { id: 'evt-night', name: 'Friday Night Whoops, round 2' } });
    await vi.advanceTimersByTimeAsync(5100);
    await waitFor(() =>
      expect(screen.getByTestId('visitor-event')).toHaveTextContent('Friday Night Whoops, round 2')
    );
    expect(stop).not.toHaveBeenCalled();
    expect(source.subscribeChanges).toHaveBeenCalledOnce();
  });

  it('says no event is open when the director closes it, and stops listening', async () => {
    const { stop, becomes } = open();
    await screen.findByTestId('race-heat');
    becomes({});
    await vi.advanceTimersByTimeAsync(5100);
    await waitFor(() =>
      expect(screen.getByTestId('visitor-event')).toHaveTextContent('No event is open')
    );
    expect(screen.queryByTestId('race-heat')).toBeNull();
    expect(stop).toHaveBeenCalledOnce();
  });

  it('picks up an event that is opened later', async () => {
    const { event: _none, ...noEvent } = HELLO;
    const { becomes } = open(noEvent, {});
    expect(screen.getByTestId('visitor-event')).toHaveTextContent('No event is open');
    becomes(NIGHT);
    await vi.advanceTimersByTimeAsync(5100);
    expect(await screen.findByTestId('race-heat')).toHaveTextContent('Qualifying Heat 1');
  });

  it('keeps what is on the page while Grid cannot be reached, and keeps asking', async () => {
    const { source } = open();
    await screen.findByTestId('race-heat');
    vi.mocked(source.getEvent).mockRejectedValue(new Error('Failed to fetch'));
    await vi.advanceTimersByTimeAsync(16_000);
    expect(screen.getByTestId('visitor-event')).toHaveTextContent('Friday Night Whoops');
    expect(screen.getByTestId('race-heat')).toBeVisible();
    expect(vi.mocked(source.getEvent).mock.calls.length).toBeGreaterThanOrEqual(4);
    // And it says that what is on the page is not live.
    expect(screen.getByTestId('visitor-offline')).toHaveTextContent(
      'Not connected to Grid right now. Trying again.'
    );
    // When Grid answers again, the notice goes.
    vi.mocked(source.getEvent).mockResolvedValue(NIGHT);
    await vi.advanceTimersByTimeAsync(5100);
    await waitFor(() => expect(screen.queryByTestId('visitor-offline')).toBeNull());
  });

  it('does not say it is offline while Grid is answering', async () => {
    open();
    await screen.findByTestId('race-heat');
    await vi.advanceTimersByTimeAsync(11_000);
    expect(screen.queryByTestId('visitor-offline')).toBeNull();
  });

  it('stops asking and listening when the page is left', async () => {
    const { source, stop } = open();
    await screen.findByTestId('race-heat');
    cleanup();
    expect(stop).toHaveBeenCalledOnce();
    const asked = vi.mocked(source.getEvent).mock.calls.length;
    await vi.advanceTimersByTimeAsync(20_000);
    expect(vi.mocked(source.getEvent).mock.calls.length).toBe(asked);
  });

  it('sends no command, whatever the visitor does on the page', async () => {
    const { source } = open();
    await screen.findByTestId('race-heat');
    for (const tabEl of screen.getAllByRole('tab')) await fireEvent.click(tabEl);
    expect(source.sendCommand).not.toHaveBeenCalled();
    expect(source.postPass).not.toHaveBeenCalled();
  });
});

describe('VisitorPage: a pilot signs in', () => {
  const askAsPilot = () => fireEvent.click(screen.getByRole('button', { name: 'Pilot' }));
  const codeBox = () => screen.getByLabelText('Your code') as HTMLInputElement;
  const form = () => screen.getByRole('form', { name: 'Pilot sign-in' });

  afterEach(() => {
    history.replaceState(null, '', '/');
  });

  it('keeps the code box away until it is asked for', async () => {
    open();
    expect(screen.queryByLabelText('Your code')).toBeNull();
    await askAsPilot();
    expect(codeBox()).toHaveAttribute('autocomplete', 'off');
    expect(codeBox()).toHaveAttribute('autocapitalize', 'characters');
  });

  it('signs in with a typed code, tidied, and then knows the pilot by name', async () => {
    const { session } = open();
    await askAsPilot();
    await fireEvent.input(codeBox(), { target: { value: ' abc-234 ' } });
    await fireEvent.submit(form());
    await waitFor(() => expect(session.signInPilot).toHaveBeenCalledWith('ABC234'));
    expect(await screen.findByTestId('pilot-signed-in')).toHaveTextContent('Signed in as Ace');
    expect(screen.queryByLabelText('Your code')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Pilot' })).toBeNull();
  });

  it('catches a slip before it is sent, so it does not count as a wrong try', async () => {
    const { session } = open();
    await askAsPilot();
    for (const [typed, words] of [
      ['', 'Type your code.'],
      ['ABC23', 'A code has 6 letters and numbers.'],
      ['ABC2345', 'A code has only 6 letters and numbers.'],
      ['ABC23O', 'never 0, O, 1 or I']
    ] as const) {
      await fireEvent.input(codeBox(), { target: { value: typed } });
      await fireEvent.submit(form());
      expect(screen.getByRole('alert')).toHaveTextContent(words);
    }
    expect(session.signInPilot).not.toHaveBeenCalled();
  });

  it('does not nag about the code while it is still being typed for the first time', async () => {
    open();
    await askAsPilot();
    await fireEvent.input(codeBox(), { target: { value: 'AB' } });
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it("shows the Director's words for a wrong code and lets the pilot try again", async () => {
    const { session } = open();
    session.signInPilot.mockRejectedValueOnce(
      new Error('That code is not right. Check it with the director.')
    );
    await askAsPilot();
    await fireEvent.input(codeBox(), { target: { value: 'AAA222' } });
    await fireEvent.submit(form());
    expect(await screen.findByRole('alert')).toHaveTextContent('That code is not right.');
    expect(screen.queryByTestId('pilot-signed-in')).toBeNull();
    await fireEvent.input(codeBox(), { target: { value: 'ABC234' } });
    await fireEvent.submit(form());
    expect(await screen.findByTestId('pilot-signed-in')).toBeVisible();
  });

  it('says so when the device has guessed too often', async () => {
    const { session } = open();
    session.signInPilot.mockRejectedValueOnce(
      new Error('Too many wrong codes from this device. Wait a minute and try again.')
    );
    await askAsPilot();
    await fireEvent.input(codeBox(), { target: { value: 'AAA222' } });
    await fireEvent.submit(form());
    expect(await screen.findByRole('alert')).toHaveTextContent('Wait a minute and try again.');
  });

  it('sends a code once however fast it is pressed', async () => {
    const { session } = open();
    let finish: (pilot: { id: string; callsign: string }) => void = () => {};
    session.signInPilot.mockImplementationOnce(() => new Promise((resolve) => (finish = resolve)));
    await askAsPilot();
    await fireEvent.input(codeBox(), { target: { value: 'ABC234' } });
    await fireEvent.submit(form());
    await fireEvent.submit(form());
    await fireEvent.submit(form());
    expect(session.signInPilot).toHaveBeenCalledOnce();
    finish({ id: 'p-ace', callsign: 'Ace' });
  });

  it("a signed-in pilot's callsign and Sign out are in the top bar, in place of the Pilot link", () => {
    open({ ...HELLO, pilot: { id: 'p-bee', callsign: 'Bee' } });
    const bar = within(screen.getByRole('banner'));
    expect(bar.getByTestId('pilot-signed-in')).toHaveTextContent('Bee');
    expect(bar.getByRole('button', { name: 'Sign out' })).toBeVisible();
    expect(bar.queryByRole('button', { name: 'Pilot' })).toBeNull();
  });

  it('the Pilot link opens the code box, and pressing it again puts it away', async () => {
    open();
    const link = screen.getByRole('button', { name: 'Pilot' });
    await fireEvent.click(link);
    expect(screen.getByLabelText('Your code')).toBeVisible();
    await fireEvent.click(link);
    expect(screen.queryByLabelText('Your code')).toBeNull();
  });

  it('is already signed in when the Director says so at load', () => {
    open({ ...HELLO, pilot: { id: 'p-bee', callsign: 'Bee' } });
    expect(screen.getByTestId('pilot-signed-in')).toHaveTextContent('Signed in as Bee');
    expect(screen.queryByRole('button', { name: 'Pilot' })).toBeNull();
  });

  it('signs out, and offers to sign in again', async () => {
    const { session } = open({ ...HELLO, pilot: { id: 'p-bee', callsign: 'Bee' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Sign out' }));
    await waitFor(() => expect(session.signOutPilot).toHaveBeenCalledOnce());
    await waitFor(() => expect(screen.queryByTestId('pilot-signed-in')).toBeNull());
    expect(screen.getByRole('button', { name: 'Pilot' })).toBeVisible();
  });

  it('a code in the address signs in by itself and is taken out of the address', async () => {
    history.replaceState(null, '', '/#/join/abc234');
    const { session } = open();
    await waitFor(() => expect(session.signInPilot).toHaveBeenCalledWith('ABC234'));
    expect(await screen.findByTestId('pilot-signed-in')).toHaveTextContent('Signed in as Ace');
    expect(location.hash).toBe('');
    expect(location.href).not.toContain('ABC234');
    expect(location.href.toLowerCase()).not.toContain('abc234');
  });

  it('a wrong code in the address shows why and opens the box to type the right one', async () => {
    history.replaceState(null, '', '/#/join/AAA222');
    const { session } = open();
    session.signInPilot.mockRejectedValueOnce(
      new Error('That code is not right. Check it with the director.')
    );
    cleanup();
    history.replaceState(null, '', '/#/join/AAA222');
    render(VisitorPage, {
      session,
      source: {
        ...fakeLiveSession(racing()),
        getEvent: vi.fn(async () => NIGHT),
        getChannels: vi.fn(async () => []),
        takeSpot: vi.fn(async () => {}),
        takeNextSpot: vi.fn(async () => {}),
        leaveSpot: vi.fn(async () => {}),
        kioskIn: vi.fn(async () => KIOSK_BEE),
        kioskDone: vi.fn(async () => {}),
        kioskSetUp: vi.fn(async () => {})
      },
      hello: HELLO,
      reload: vi.fn()
    });
    expect(await screen.findByRole('alert')).toHaveTextContent('That code is not right.');
    expect(codeBox()).toBeVisible();
    expect(location.hash).toBe('');
  });

  it('an address that is not a sign-in link is left alone and signs nobody in', async () => {
    for (const hash of ['#/join/ABC23', '#/join/', '#/events', '#/join/ABC234/x']) {
      history.replaceState(null, '', `/${hash}`);
      const { session } = open();
      await vi.advanceTimersByTimeAsync(50);
      expect(session.signInPilot, hash).not.toHaveBeenCalled();
      expect(location.hash).toBe(hash);
      cleanup();
    }
  });

  it('offers no pilot sign-in when no event is open', () => {
    const { event: _none, ...noEvent } = HELLO;
    open(noEvent, {});
    expect(screen.queryByRole('button', { name: 'Pilot' })).toBeNull();
  });

  it('cancel puts the box away and forgets what went wrong', async () => {
    const { session } = open();
    session.signInPilot.mockRejectedValueOnce(new Error('That code is not right.'));
    await askAsPilot();
    await fireEvent.input(codeBox(), { target: { value: 'AAA222' } });
    await fireEvent.submit(form());
    await screen.findByRole('alert');
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByLabelText('Your code')).toBeNull();
    await askAsPilot();
    expect(screen.queryByRole('alert')).toBeNull();
  });
});

describe('VisitorPage: what a signed-in pilot gets', () => {
  it('a spectator sees no Me tab; a pilot who signs in gets one, with their heat and channel', async () => {
    const { source } = open();
    await screen.findByTestId('race-heat');
    expect(screen.queryByRole('tab', { name: /^Me/ })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Pilot' }));
    await fireEvent.input(screen.getByLabelText('Your code'), { target: { value: 'ABC234' } });
    await fireEvent.submit(screen.getByRole('form', { name: 'Pilot sign-in' }));
    const me = await screen.findByRole('tab', { name: /^Me/ });
    await waitFor(() => expect(me).toHaveAttribute('aria-selected', 'true'));
    // Ace is in the race that is on: the page says so, with the seat's channel.
    const alert = await screen.findByTestId('pilot-alert');
    expect(alert).toHaveTextContent('You are flying now');
    await waitFor(() => expect(alert).toHaveTextContent('R1 · 5658 MHz'));
    expect(source.getChannels).toHaveBeenCalled();
  });

  it('asks for the channels again now and then, and keeps the last answer when that fails', async () => {
    const { source } = open({ ...HELLO, pilot: { id: 'p-ace', callsign: 'Ace' } });
    const alert = await screen.findByTestId('pilot-alert');
    await waitFor(() => expect(alert).toHaveTextContent('R1 · 5658 MHz'));
    const asked = vi.mocked(source.getChannels).mock.calls.length;
    vi.mocked(source.getChannels).mockRejectedValue(new Error('Failed to fetch'));
    await vi.advanceTimersByTimeAsync(21_000);
    expect(vi.mocked(source.getChannels).mock.calls.length).toBeGreaterThan(asked);
    expect(screen.getByTestId('pilot-alert')).toHaveTextContent('R1 · 5658 MHz');
  });

  it('asks for no channels while no event is open', async () => {
    const { event: _none, ...noEvent } = HELLO;
    const { source } = open(noEvent, {});
    await vi.advanceTimersByTimeAsync(25_000);
    expect(source.getChannels).not.toHaveBeenCalled();
  });

  it('signing out takes the Me tab and the alert away', async () => {
    open({ ...HELLO, pilot: { id: 'p-ace', callsign: 'Ace' } });
    await screen.findByTestId('pilot-alert');
    await fireEvent.click(screen.getByRole('button', { name: 'Sign out' }));
    await waitFor(() => expect(screen.queryByTestId('pilot-alert')).toBeNull());
    expect(screen.queryByRole('tab', { name: /^Me/ })).toBeNull();
  });
});

describe('VisitorPage: signing up for a heat', () => {
  /** The open event has a sign-up phase. Ace holds seat 1 of heat 1. */
  function openSheet(hello: PublicHello = HELLO) {
    const session = {
      signInDirector: vi.fn(async (_token: string) => {}),
      signInPilot: vi.fn(async (_code: string) => ({ id: 'p-bee', callsign: 'Bee' })),
      signOutPilot: vi.fn(async () => {})
    };
    const view = signupEvent(signupSheet());
    const source: PublicSource = {
      ...fakeLiveSession(view, {
        listPilots: vi.fn(async () => SIGNUP_PILOTS),
        listTimers: vi.fn(async () => [GATE])
      }),
      getEvent: vi.fn(async () => NIGHT),
      getChannels: vi.fn(async () => []),
      takeSpot: vi.fn(async () => {}),
      takeNextSpot: vi.fn(async () => {}),
      leaveSpot: vi.fn(async () => {}),
      kioskIn: vi.fn(async () => KIOSK_BEE),
      kioskDone: vi.fn(async () => {}),
      kioskSetUp: vi.fn(async () => {})
    };
    render(VisitorPage, { session, source, hello, reload: vi.fn() });
    return { source };
  }
  const signUpTab = () => screen.findByRole('tab', { name: 'Sign up' });

  it('has no Sign up tab when the event has nothing to sign up for', async () => {
    open();
    await screen.findByTestId('race-heat');
    expect(screen.queryByRole('tab', { name: 'Sign up' })).toBeNull();
  });

  it('someone who is only looking gets no Sign up tab and sees no sheet', async () => {
    openSheet();
    await screen.findByRole('tab', { name: 'Results' });
    expect(screen.queryByRole('tab', { name: 'Sign up' })).toBeNull();
    expect(screen.queryByRole('region', { name: 'Sign-up for Qualifying' })).toBeNull();
    expect(screen.getAllByRole('tab').map((el) => el.textContent?.trim())).toEqual([
      'Last race',
      'Up next',
      'Results'
    ]);
  });

  it('a signed-in pilot takes a seat, and the page reads the event again', async () => {
    const { source } = openSheet({ ...HELLO, pilot: { id: 'p-bee', callsign: 'Bee' } });
    await fireEvent.click(await signUpTab());
    const reads = vi.mocked(source.getEventState).mock.calls.length;
    await fireEvent.click(screen.getByRole('button', { name: 'Take Seat 2 in Qualifying Heat 1' }));
    await waitFor(() =>
      expect(source.takeSpot).toHaveBeenCalledWith('quali/heat-1', { adapter: 'gate', seat: '1' })
    );
    await waitFor(() =>
      expect(vi.mocked(source.getEventState).mock.calls.length).toBeGreaterThan(reads)
    );
  });

  it('a signed-in pilot leaves their seat and takes the next free one', async () => {
    const { source } = openSheet({ ...HELLO, pilot: { id: 'p-ace', callsign: 'Ace' } });
    await fireEvent.click(await signUpTab());
    await fireEvent.click(screen.getByRole('button', { name: 'Leave Qualifying Heat 1' }));
    await waitFor(() => expect(source.leaveSpot).toHaveBeenCalledWith('quali/heat-1'));
    await fireEvent.click(screen.getByRole('button', { name: 'Next free spot on Seat 3' }));
    await waitFor(() =>
      expect(source.takeNextSpot).toHaveBeenCalledWith('quali', { adapter: 'gate', seat: '2' })
    );
  });

  it("shows the Director's reason when it refuses, and does not read the event for nothing", async () => {
    const { source } = openSheet({ ...HELLO, pilot: { id: 'p-bee', callsign: 'Bee' } });
    vi.mocked(source.takeSpot).mockRejectedValueOnce(
      new Error('that seat in Qualifying Heat 1 is already taken')
    );
    await fireEvent.click(await signUpTab());
    await fireEvent.click(screen.getByRole('button', { name: 'Take Seat 2 in Qualifying Heat 1' }));
    expect(await screen.findByTestId('signup-error')).toHaveTextContent('already taken');
  });

  it('signing out takes the Sign up tab and the sheet away', async () => {
    openSheet({ ...HELLO, pilot: { id: 'p-bee', callsign: 'Bee' } });
    await fireEvent.click(await signUpTab());
    expect(screen.getAllByRole('button', { name: /^Take/ }).length).toBeGreaterThan(0);
    await fireEvent.click(screen.getByRole('button', { name: 'Sign out' }));
    await waitFor(() => expect(screen.queryByRole('tab', { name: 'Sign up' })).toBeNull());
    expect(screen.queryByRole('button', { name: /^Take/ })).toBeNull();
    expect(screen.queryByRole('region', { name: 'Sign-up for Qualifying' })).toBeNull();
    // The page falls back to the race, not to nothing.
    expect(screen.getByRole('tab', { name: 'Last race' })).toHaveAttribute('aria-selected', 'true');
  });

  it('opened as a kiosk it is the kiosk page, with none of the visitor page', async () => {
    location.hash = '#/kiosk';
    openSheet();
    // Not set up by the director: it asks for a kiosk code, and shows no race.
    expect(await screen.findByLabelText('Kiosk code')).toBeVisible();
    expect(screen.queryByRole('tablist')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Pilot' })).toBeNull();
    cleanup();
    // Set up: it waits for a pilot's code.
    location.hash = '#/kiosk';
    openSheet({ ...HELLO, kiosk: true });
    expect(await screen.findByLabelText('Type your code')).toBeVisible();
    expect(screen.queryByRole('tablist')).toBeNull();
  });

  it('a sign-in link opened on a tablet does not make it a kiosk', async () => {
    location.hash = '#/join/ABC234';
    openSheet({ ...HELLO, kiosk: true });
    expect(await screen.findByTestId('pilot-signed-in')).toHaveTextContent('Bee');
    expect(screen.queryByLabelText('Type your code')).toBeNull();
  });
});
