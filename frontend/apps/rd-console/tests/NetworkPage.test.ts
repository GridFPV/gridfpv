/**
 * The Network page: the switch that lets other devices open Grid, the address to give out,
 * and the port.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { KioskView, NetworkRequest, NetworkView, PublicHello } from '@gridfpv/types';
import NetworkPage from '../src/screens/NetworkPage.svelte';

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const OFF: NetworkView = { enabled: false, port: 8081, state: 'Off', addresses: [] };
const ON: NetworkView = {
  enabled: true,
  port: 8081,
  state: 'On',
  addresses: ['http://192.168.1.20:8081/']
};
const HELLO: PublicHello = {
  version: '0.5.1',
  caller: 'Local',
  event: 'Friday Night Whoops',
  director_sign_in: false,
  kiosk: false
};

function open(
  first: NetworkView,
  hello: PublicHello = HELLO,
  firstKiosks: KioskView = { codes: [], kiosks: [] }
) {
  let now = first;
  let kiosks = firstKiosks;
  const session = {
    getNetwork: vi.fn(async () => now),
    putNetwork: vi.fn(async (request: NetworkRequest): Promise<NetworkView | undefined> => {
      now = {
        enabled: request.enabled,
        port: request.port ?? now.port,
        state: request.enabled ? 'Starting' : 'Off',
        addresses: []
      };
      return now;
    }),
    hello: vi.fn(async () => hello),
    signOutDirector: vi.fn(async () => {}),
    getKiosks: vi.fn(async (): Promise<KioskView> => kiosks),
    newKioskCode: vi.fn(async (): Promise<KioskView | undefined> => {
      kiosks = { ...kiosks, codes: [...kiosks.codes, `NEWC${2345 + kiosks.codes.length}`] };
      return kiosks;
    }),
    cancelKioskCode: vi.fn(async (code: string): Promise<KioskView | undefined> => {
      kiosks = { ...kiosks, codes: kiosks.codes.filter((kept) => kept !== code) };
      return kiosks;
    }),
    removeKiosk: vi.fn(async (number: number): Promise<KioskView | undefined> => {
      kiosks = { ...kiosks, kiosks: kiosks.kiosks.filter((kept) => kept.number !== number) };
      return kiosks;
    })
  };
  render(NetworkPage, { session });
  return {
    session,
    /** What the Director says from now on. */
    becomes: (next: NetworkView) => (now = next)
  };
}

const state = async () => (await screen.findByTestId('network-state')).textContent;

describe('NetworkPage', () => {
  it('starts off, says only this computer can open Grid, and gives no address', async () => {
    open(OFF);
    expect(await state()).toBe('Off');
    expect(screen.getByText('Only this computer can open Grid.')).toBeVisible();
    expect(screen.queryByRole('list', { name: 'Addresses for other devices' })).toBeNull();
    expect(screen.getByRole('button', { name: 'Let other devices open Grid' })).toBeEnabled();
    expect(screen.queryByRole('button', { name: 'Turn off' })).toBeNull();
  });

  it('says plainly what other devices can and cannot do', async () => {
    open(OFF);
    await state();
    const card = screen.getByText('What other devices can do').closest('section')!;
    expect(card).toHaveTextContent('They can look.');
    expect(card).toHaveTextContent('They cannot start or stop a race, change a lap');
    expect(card).toHaveTextContent('Only this computer can do those things.');
  });

  it('turns on, shows it starting, then the address once the listener is up', async () => {
    const { session, becomes } = open(OFF);
    await fireEvent.click(
      await screen.findByRole('button', { name: 'Let other devices open Grid' })
    );
    expect(session.putNetwork).toHaveBeenCalledWith({ enabled: true });
    await waitFor(async () => expect(await state()).toBe('Starting'));
    expect(screen.queryByRole('list', { name: 'Addresses for other devices' })).toBeNull();
    // The listener comes up a moment later; the page finds out by itself.
    becomes(ON);
    await vi.advanceTimersByTimeAsync(1100);
    await waitFor(async () => expect(await state()).toBe('On'));
    const list = screen.getByRole('list', { name: 'Addresses for other devices' });
    const link = within(list).getByRole('link', { name: 'http://192.168.1.20:8081/' });
    expect(link).toHaveAttribute('href', 'http://192.168.1.20:8081/');
    expect(screen.getByText('Other devices on this network can open Grid.')).toBeVisible();
    expect(screen.getByRole('button', { name: 'Turn off' })).toBeEnabled();
  });

  it('turns off, and the address goes away', async () => {
    const { session } = open(ON);
    await fireEvent.click(await screen.findByRole('button', { name: 'Turn off' }));
    expect(session.putNetwork).toHaveBeenCalledWith({ enabled: false });
    await waitFor(async () => expect(await state()).toBe('Off'));
    expect(screen.queryByRole('list', { name: 'Addresses for other devices' })).toBeNull();
  });

  it('copies an address', async () => {
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
    open(ON);
    await fireEvent.click(await screen.findByRole('button', { name: 'Copy' }));
    expect(writeText).toHaveBeenCalledWith('http://192.168.1.20:8081/');
  });

  it('lists every address and says which to use when there are two', async () => {
    open({ ...ON, addresses: ['http://192.168.1.20:8081/', 'http://10.42.0.1:8081/'] });
    const list = await screen.findByRole('list', { name: 'Addresses for other devices' });
    expect(within(list).getAllByRole('link')).toHaveLength(2);
    expect(screen.getByText(/on more than one network/)).toBeVisible();
  });

  it('does not say "more than one network" for one address', async () => {
    open(ON);
    await state();
    expect(screen.queryByText(/on more than one network/)).toBeNull();
  });

  it('shows why the listener is not working, and still lets it be turned off', async () => {
    open({
      enabled: true,
      port: 8081,
      state: 'Failed',
      addresses: [],
      problem: 'Port 8081 is in use by another program on this computer. Pick another port.'
    });
    expect(await state()).toBe('Not working');
    expect(screen.getByTestId('network-problem')).toHaveTextContent('Port 8081 is in use');
    expect(screen.queryByRole('list', { name: 'Addresses for other devices' })).toBeNull();
    expect(screen.getByRole('button', { name: 'Turn off' })).toBeEnabled();
  });

  it('on with no address says the computer is not on a network', async () => {
    open({
      ...ON,
      addresses: [],
      problem: 'This computer is not on a network, so there is no address to give out.'
    });
    expect(await state()).toBe('On');
    expect(screen.getByTestId('network-problem')).toHaveTextContent('not on a network');
    expect(screen.queryByRole('list', { name: 'Addresses for other devices' })).toBeNull();
    expect(screen.queryByText('Other devices on this network can open Grid.')).toBeNull();
  });

  it('a Director started on the network has no switch and no port to change', async () => {
    open({ enabled: false, port: 8090, state: 'Always', addresses: ['http://192.168.1.20:8090/'] });
    expect(await state()).toBe('On');
    expect(screen.getByText(/this is always on/)).toBeVisible();
    expect(screen.queryByRole('button', { name: 'Let other devices open Grid' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Turn off' })).toBeNull();
    expect(screen.queryByLabelText('Port')).toBeNull();
    expect(screen.getByRole('link', { name: 'http://192.168.1.20:8090/' })).toBeVisible();
  });

  it('changes the port and keeps the switch as it was', async () => {
    const { session } = open(ON);
    const port = (await screen.findByLabelText('Port')) as HTMLInputElement;
    expect(port.value).toBe('8081');
    const change = screen.getByRole('button', { name: 'Change port' });
    expect(change).toBeDisabled();
    await fireEvent.input(port, { target: { value: '9000' } });
    expect(change).toBeEnabled();
    await fireEvent.click(change);
    expect(session.putNetwork).toHaveBeenCalledWith({ enabled: true, port: 9000 });
    await waitFor(() => expect(port.value).toBe('9000'));
    // Off stays off when only the port is changed.
    cleanup();
    const off = open(OFF);
    const other = (await screen.findByLabelText('Port')) as HTMLInputElement;
    await fireEvent.input(other, { target: { value: '9001' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Change port' }));
    expect(off.session.putNetwork).toHaveBeenCalledWith({ enabled: false, port: 9001 });
  });

  it('refuses a port that cannot work before sending anything', async () => {
    const { session } = open(OFF);
    const port = (await screen.findByLabelText('Port')) as HTMLInputElement;
    const change = screen.getByRole('button', { name: 'Change port' });
    for (const [typed, words] of [
      ['80', 'Pick a port from 1024 to 65535.'],
      ['65536', 'Pick a port from 1024 to 65535.'],
      ['abc', 'A port is a whole number.'],
      ['', 'Type a port number.']
    ] as const) {
      await fireEvent.input(port, { target: { value: typed } });
      expect(screen.getByRole('alert')).toHaveTextContent(words);
      expect(change).toBeDisabled();
      // Enter in the box does not get round the button.
      await fireEvent.submit(screen.getByRole('form', { name: 'Port for other devices' }));
    }
    expect(session.putNetwork).not.toHaveBeenCalled();
    // The same port as now is not a change.
    await fireEvent.input(port, { target: { value: '8081' } });
    expect(screen.queryByRole('alert')).toBeNull();
    expect(change).toBeDisabled();
  });

  it('keeps what is being typed in the port box when the page reads the setting again', async () => {
    open(OFF);
    const port = (await screen.findByLabelText('Port')) as HTMLInputElement;
    await fireEvent.input(port, { target: { value: '90' } });
    await vi.advanceTimersByTimeAsync(11_000);
    expect(port.value).toBe('90');
  });

  it("shows the Director's words when a change is refused, and changes nothing", async () => {
    const { session } = open(OFF);
    session.putNetwork.mockRejectedValueOnce(
      new Error('Port 8080 is the one Grid itself uses on this computer. Pick another.')
    );
    const port = (await screen.findByLabelText('Port')) as HTMLInputElement;
    await fireEvent.input(port, { target: { value: '8080' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Change port' }));
    expect(await screen.findByText(/Port 8080 is the one Grid itself uses/)).toBeVisible();
    expect(await state()).toBe('Off');
    // The next try that works clears the message.
    await fireEvent.input(port, { target: { value: '9000' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Change port' }));
    await waitFor(() => expect(screen.queryByText(/Port 8080 is the one/)).toBeNull());
  });

  it('a token prompt that is cancelled changes nothing', async () => {
    const { session } = open(OFF);
    session.putNetwork.mockResolvedValueOnce(undefined);
    await fireEvent.click(
      await screen.findByRole('button', { name: 'Let other devices open Grid' })
    );
    await waitFor(() => expect(session.putNetwork).toHaveBeenCalledOnce());
    expect(await state()).toBe('Off');
    expect(screen.getByRole('button', { name: 'Let other devices open Grid' })).toBeEnabled();
  });

  it('says so when the setting cannot be read, and recovers when it can', async () => {
    const { session } = open(OFF);
    session.getNetwork.mockRejectedValueOnce(new Error('Could not reach Grid.'));
    cleanup();
    render(NetworkPage, { session });
    expect(await screen.findByText('Grid could not read its network setting.')).toBeVisible();
    await vi.advanceTimersByTimeAsync(5100);
    await waitFor(() =>
      expect(screen.queryByText('Grid could not read its network setting.')).toBeNull()
    );
    expect(await state()).toBe('Off');
  });

  it('notices a new address when the computer changes network', async () => {
    const { becomes } = open(ON);
    await screen.findByRole('link', { name: 'http://192.168.1.20:8081/' });
    becomes({ ...ON, addresses: ['http://10.42.0.1:8081/'] });
    await vi.advanceTimersByTimeAsync(5100);
    expect(await screen.findByRole('link', { name: 'http://10.42.0.1:8081/' })).toBeVisible();
    expect(screen.queryByRole('link', { name: 'http://192.168.1.20:8081/' })).toBeNull();
  });

  it('stops reading the setting when the page is left', async () => {
    const { session } = open(ON);
    await state();
    cleanup();
    const before = session.getNetwork.mock.calls.length;
    await vi.advanceTimersByTimeAsync(20_000);
    expect(session.getNetwork.mock.calls.length).toBe(before);
  });

  it('offers sign-out only on a device signed in with the access token', async () => {
    open(ON);
    await state();
    expect(screen.queryByRole('button', { name: 'Sign out on this device' })).toBeNull();
    cleanup();
    const { session } = open(ON, { ...HELLO, caller: 'Director', director_sign_in: true });
    const out = await screen.findByRole('button', { name: 'Sign out on this device' });
    const reload = vi.fn();
    Object.defineProperty(window, 'location', {
      configurable: true,
      value: { ...window.location, reload }
    });
    await fireEvent.click(out);
    await waitFor(() => expect(session.signOutDirector).toHaveBeenCalledOnce());
    await waitFor(() => expect(reload).toHaveBeenCalledOnce());
  });

  it('gives the kiosk address only when other devices can reach Grid', async () => {
    open({ ...ON, addresses: ['http://192.168.1.20:8081/', 'http://10.42.0.1:8081/'] });
    const link = await screen.findByTestId('kiosk-link');
    expect(link).toHaveTextContent('http://192.168.1.20:8081/#/kiosk');
    expect(link).toHaveAttribute('href', 'http://192.168.1.20:8081/#/kiosk');
    cleanup();
    open(OFF);
    await screen.findByTestId('network-state');
    expect(screen.queryByTestId('kiosk-link')).toBeNull();
    expect(screen.getByText(/Let other devices open Grid first\./)).toBeVisible();
    cleanup();
    // On, but this computer has no address to give.
    open({ ...ON, addresses: [] });
    await screen.findByTestId('network-state');
    expect(screen.queryByTestId('kiosk-link')).toBeNull();
  });
});

describe('NetworkPage: kiosks', () => {
  const codes = () => screen.queryAllByTestId('kiosk-code').map((el) => el.textContent?.trim());
  const TWO: KioskView = {
    codes: ['ABCD2345', 'WXYZ6789'],
    kiosks: [
      { number: 1, set_up_at: 1_794_800_000 },
      { number: 3, set_up_at: 1_794_803_600 }
    ]
  };

  it('starts with no codes and no kiosks, and says what a kiosk is', async () => {
    open(ON);
    await screen.findByTestId('kiosk-link');
    expect(screen.getByText(/Only a tablet you set up here can be one\./)).toBeVisible();
    expect(codes()).toEqual([]);
    expect(screen.queryByRole('list', { name: 'Kiosks set up' })).toBeNull();
    expect(screen.queryByRole('list', { name: 'Kiosk codes waiting to be used' })).toBeNull();
  });

  it('makes a code each time it is asked, and shows each in two halves', async () => {
    const { session } = open(ON);
    const make = await screen.findByRole('button', { name: 'New kiosk code' });
    await fireEvent.click(make);
    await waitFor(() => expect(codes()).toEqual(['NEWC 2345']));
    await fireEvent.click(make);
    await waitFor(() => expect(codes()).toEqual(['NEWC 2345', 'NEWC 2346']));
    expect(session.newKioskCode).toHaveBeenCalledTimes(2);
  });

  it('lists the codes waiting and the kiosks set up', async () => {
    open(ON, HELLO, TWO);
    await waitFor(() => expect(codes()).toEqual(['ABCD 2345', 'WXYZ 6789']));
    const set = within(screen.getByRole('list', { name: 'Kiosks set up' })).getAllByRole(
      'listitem'
    );
    expect(set.map((item) => item.textContent?.replace(/\s+/g, ' ').trim().slice(0, 7))).toEqual([
      'Kiosk 1',
      'Kiosk 3'
    ]);
    expect(set[0]).toHaveTextContent(/set up .+\d/);
  });

  it('cancels a code that has not been used', async () => {
    const { session } = open(ON, HELLO, TWO);
    await fireEvent.click(
      await screen.findByRole('button', { name: 'Cancel kiosk code ABCD 2345' })
    );
    await waitFor(() => expect(codes()).toEqual(['WXYZ 6789']));
    expect(session.cancelKioskCode).toHaveBeenCalledWith('ABCD2345');
  });

  it('removes a kiosk', async () => {
    const { session } = open(ON, HELLO, TWO);
    await fireEvent.click(await screen.findByRole('button', { name: 'Remove kiosk 3' }));
    await waitFor(() =>
      expect(screen.queryByRole('button', { name: 'Remove kiosk 3' })).toBeNull()
    );
    expect(session.removeKiosk).toHaveBeenCalledWith(3);
    expect(screen.getByRole('button', { name: 'Remove kiosk 1' })).toBeVisible();
  });

  it('a code a tablet has used moves to the kiosks by itself', async () => {
    const { session } = open(ON, HELLO, { codes: ['ABCD2345'], kiosks: [] });
    await waitFor(() => expect(codes()).toEqual(['ABCD 2345']));
    session.getKiosks.mockResolvedValue({
      codes: [],
      kiosks: [{ number: 1, set_up_at: 1_794_800_000 }]
    });
    await vi.advanceTimersByTimeAsync(6_000);
    await waitFor(() => expect(codes()).toEqual([]));
    expect(screen.getByRole('button', { name: 'Remove kiosk 1' })).toBeVisible();
  });

  it("shows the Director's reason when a change is refused, and keeps the list", async () => {
    const { session } = open(ON, HELLO, TWO);
    session.newKioskCode.mockRejectedValueOnce(
      new Error('There are already a lot of kiosk codes waiting. Use one or cancel one first.')
    );
    await fireEvent.click(await screen.findByRole('button', { name: 'New kiosk code' }));
    expect(await screen.findByText(/already a lot of kiosk codes waiting/)).toBeVisible();
    expect(codes()).toEqual(['ABCD 2345', 'WXYZ 6789']);
  });

  it('a prompt that was cancelled changes nothing', async () => {
    const { session } = open(ON, HELLO, TWO);
    session.removeKiosk.mockResolvedValueOnce(undefined);
    await fireEvent.click(await screen.findByRole('button', { name: 'Remove kiosk 1' }));
    await waitFor(() => expect(session.removeKiosk).toHaveBeenCalledOnce());
    expect(screen.getByRole('button', { name: 'Remove kiosk 1' })).toBeVisible();
  });

  it('keeps the list when it cannot be read again', async () => {
    const { session } = open(ON, HELLO, TWO);
    await waitFor(() => expect(codes()).toHaveLength(2));
    session.getKiosks.mockRejectedValue(new Error('offline'));
    await vi.advanceTimersByTimeAsync(6_000);
    expect(codes()).toEqual(['ABCD 2345', 'WXYZ 6789']);
  });

  it('can make a code while other devices are shut out, for setting up ahead', async () => {
    const { session } = open(OFF);
    await fireEvent.click(await screen.findByRole('button', { name: 'New kiosk code' }));
    await waitFor(() => expect(session.newKioskCode).toHaveBeenCalledOnce());
  });
});
