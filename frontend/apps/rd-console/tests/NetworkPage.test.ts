/**
 * The Network page: the two switches that let other devices in (Public and Remote RD), the
 * address and port of each, and the access token.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type {
  Door,
  DoorRequest,
  DoorView,
  KioskView,
  NetworkView,
  PublicHello
} from '@gridfpv/types';
import NetworkPage from '../src/screens/NetworkPage.svelte';

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const SHUT_PUBLIC: DoorView = {
  enabled: false,
  port: 80,
  port_picked: false,
  state: 'Off',
  addresses: []
};
const SHUT_REMOTE: DoorView = { ...SHUT_PUBLIC, port: 8082 };
const OPEN_PUBLIC: DoorView = {
  enabled: true,
  port: 80,
  port_picked: false,
  state: 'On',
  addresses: ['http://192.168.1.20/']
};
const OPEN_REMOTE: DoorView = {
  enabled: true,
  port: 8082,
  port_picked: false,
  state: 'On',
  addresses: ['http://192.168.1.20:8082/']
};
const TOKEN = 'K7QM-2XWD-9HRA';

/** Both doors shut, and no token made yet. */
const OFF: NetworkView = { public: SHUT_PUBLIC, remote: SHUT_REMOTE };
/** The Public door open, the Remote RD door shut. */
const ON: NetworkView = { public: OPEN_PUBLIC, remote: SHUT_REMOTE };
/** Both open, read on the Grid computer. */
const BOTH: NetworkView = { public: OPEN_PUBLIC, remote: OPEN_REMOTE, token: TOKEN };

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
    putDoor: vi.fn(async (door: Door, request: DoorRequest): Promise<NetworkView | undefined> => {
      const key = door === 'Public' ? 'public' : 'remote';
      const before = now[key];
      const usual = door === 'Public' ? 80 : 8082;
      const port =
        request.port === undefined ? before.port : request.port === 0 ? usual : request.port;
      now = {
        ...now,
        [key]: {
          enabled: request.enabled,
          port,
          port_picked: request.port === undefined ? before.port_picked : request.port !== 0,
          state: request.enabled ? 'Starting' : 'Off',
          addresses: []
        },
        // Opening the Remote RD door makes a token when there is none.
        ...(door === 'Remote' && request.enabled && !now.token ? { token: TOKEN } : {})
      };
      return now;
    }),
    putAccessToken: vi.fn(async (typed?: string): Promise<NetworkView | undefined> => {
      now = { ...now, token: typed ?? 'NEW2-NEW3-NEW4' };
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
    renameKiosk: vi.fn(async (number: number, name: string): Promise<KioskView | undefined> => {
      kiosks = {
        ...kiosks,
        kiosks: kiosks.kiosks.map((kept) => {
          if (kept.number !== number) return kept;
          const { name: _old, ...rest } = kept;
          return name === '' ? rest : { ...rest, name };
        })
      };
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

/** One door's card, once the page has read the setting. */
const card = async (door: 'public' | 'remote-rd') => await screen.findByTestId(`door-${door}`);
const toggle = (name: 'Public' | 'Remote RD') =>
  screen.getByRole('switch', { name: `Enable ${name}` }) as HTMLInputElement;
const addresses = (name: 'Public' | 'Remote RD') =>
  screen.queryByRole('list', { name: `${name} addresses` });

describe('NetworkPage', () => {
  it('has two switches, Enable Public and Enable Remote RD, both off to start with', async () => {
    open(OFF);
    await card('public');
    expect(toggle('Public')).not.toBeChecked();
    expect(toggle('Remote RD')).not.toBeChecked();
    expect(addresses('Public')).toBeNull();
    expect(addresses('Remote RD')).toBeNull();
    // No token is shown before there is one.
    expect(screen.queryByTestId('access-token')).toBeNull();
  });

  it('says who each door is for, and nothing about what visitors cannot do', async () => {
    open(OFF);
    expect(await card('public')).toHaveTextContent('Spectators, pilots and kiosks');
    expect(await card('remote-rd')).toHaveTextContent('Run Grid from another device');
    expect(screen.queryByText('What other devices can do')).toBeNull();
    expect(document.body.textContent).not.toMatch(/They can look/);
  });

  it('says nothing about a door that is simply on or off', async () => {
    open(ON);
    await card('public');
    expect(screen.queryByTestId('door-state')).toBeNull();
  });

  it('turns Public on, shows it starting, then the address once the listener is up', async () => {
    const { session, becomes } = open(OFF);
    await card('public');
    await fireEvent.click(toggle('Public'));
    expect(session.putDoor).toHaveBeenCalledWith('Public', { enabled: true });
    const pub = await card('public');
    await waitFor(() =>
      expect(within(pub).getByTestId('door-state')).toHaveTextContent('Starting')
    );
    expect(toggle('Public')).toBeChecked();
    expect(toggle('Remote RD')).not.toBeChecked();
    expect(addresses('Public')).toBeNull();
    // The listener comes up a moment later; the page finds out by itself.
    becomes(ON);
    await vi.advanceTimersByTimeAsync(1100);
    const link = await screen.findByRole('link', { name: '192.168.1.20' });
    expect(link).toHaveAttribute('href', 'http://192.168.1.20/');
    expect(screen.queryByTestId('door-state')).toBeNull();
  });

  it('turns Public off, and the address goes away', async () => {
    const { session } = open(ON);
    await card('public');
    await fireEvent.click(toggle('Public'));
    expect(session.putDoor).toHaveBeenCalledWith('Public', { enabled: false });
    await waitFor(() => expect(addresses('Public')).toBeNull());
    expect(toggle('Public')).not.toBeChecked();
  });

  it('turns Remote RD on by its own switch, and the token appears', async () => {
    const { session } = open(ON);
    await card('remote-rd');
    await fireEvent.click(toggle('Remote RD'));
    expect(session.putDoor).toHaveBeenCalledWith('Remote', { enabled: true });
    expect(await screen.findByTestId('access-token')).toHaveTextContent(TOKEN);
    // The other door is left as it was.
    expect(toggle('Public')).toBeChecked();
    expect(session.putDoor).toHaveBeenCalledOnce();
  });

  it('shows each door its own address and port', async () => {
    open(BOTH);
    const pub = await card('public');
    const remote = await card('remote-rd');
    expect(within(pub).getByRole('link', { name: '192.168.1.20' })).toBeVisible();
    expect(within(remote).getByRole('link', { name: '192.168.1.20:8082' })).toHaveAttribute(
      'href',
      'http://192.168.1.20:8082/'
    );
    expect((within(pub).getByLabelText('Port') as HTMLInputElement).value).toBe('80');
    expect((within(remote).getByLabelText('Port') as HTMLInputElement).value).toBe('8082');
  });

  it('copies an address', async () => {
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
    open(ON);
    await fireEvent.click(await screen.findByRole('button', { name: 'Copy 192.168.1.20' }));
    expect(writeText).toHaveBeenCalledWith('http://192.168.1.20/');
  });

  it('lists every address and says which to use when there are two', async () => {
    open({
      ...ON,
      public: { ...OPEN_PUBLIC, addresses: ['http://192.168.1.20/', 'http://10.42.0.1/'] }
    });
    await card('public');
    expect(within(addresses('Public')!).getAllByRole('link')).toHaveLength(2);
    expect(screen.getByText(/Use the first address/)).toBeVisible();
  });

  it('does not say which address to use when there is one', async () => {
    open(ON);
    await card('public');
    expect(screen.queryByText(/Use the first address/)).toBeNull();
  });

  it('says so when Grid is on its second choice of port', async () => {
    open({
      ...ON,
      public: {
        ...OPEN_PUBLIC,
        port: 8081,
        addresses: ['http://192.168.1.20:8081/'],
        note: 'This computer did not let Grid use port 80, so it is using port 8081.'
      }
    });
    const pub = await card('public');
    expect(within(pub).getByTestId('door-note')).toHaveTextContent('using port 8081');
    expect((within(pub).getByLabelText('Port') as HTMLInputElement).value).toBe('8081');
    expect(within(pub).getByRole('link', { name: '192.168.1.20:8081' })).toBeVisible();
  });

  it('shows why a listener is not working, on its own card, and still lets it be turned off', async () => {
    const { session } = open({
      ...BOTH,
      remote: {
        ...OPEN_REMOTE,
        state: 'Failed',
        addresses: [],
        problem: 'Port 8082 is in use by another program on this computer. Pick another port.'
      }
    });
    const remote = await card('remote-rd');
    expect(within(remote).getByTestId('door-state')).toHaveTextContent('Not working');
    expect(within(remote).getByTestId('door-problem')).toHaveTextContent('Port 8082 is in use');
    expect(addresses('Remote RD')).toBeNull();
    // The other door is fine and says nothing.
    const pub = await card('public');
    expect(within(pub).queryByTestId('door-state')).toBeNull();
    expect(within(pub).queryByTestId('door-problem')).toBeNull();
    await fireEvent.click(toggle('Remote RD'));
    expect(session.putDoor).toHaveBeenCalledWith('Remote', { enabled: false });
  });

  it('on with no address says the computer is not on a network', async () => {
    open({
      ...ON,
      public: {
        ...OPEN_PUBLIC,
        addresses: [],
        problem: 'This computer is not on a network, so there is no address to give out.'
      }
    });
    const pub = await card('public');
    expect(within(pub).getByTestId('door-problem')).toHaveTextContent('not on a network');
    expect(addresses('Public')).toBeNull();
  });

  it('changes a port and keeps the switch as it was', async () => {
    const { session } = open(BOTH);
    const remote = await card('remote-rd');
    const port = within(remote).getByLabelText('Port') as HTMLInputElement;
    // Nothing to press until the number is changed.
    expect(within(remote).queryByRole('button', { name: 'Change port' })).toBeNull();
    await fireEvent.input(port, { target: { value: '9000' } });
    await fireEvent.click(within(remote).getByRole('button', { name: 'Change port' }));
    expect(session.putDoor).toHaveBeenCalledWith('Remote', { enabled: true, port: 9000 });
    await waitFor(() => expect(port.value).toBe('9000'));
    // Off stays off when only the port is changed.
    cleanup();
    const off = open(OFF);
    const pub = await card('public');
    await fireEvent.input(within(pub).getByLabelText('Port'), { target: { value: '9001' } });
    await fireEvent.click(within(pub).getByRole('button', { name: 'Change port' }));
    expect(off.session.putDoor).toHaveBeenCalledWith('Public', { enabled: false, port: 9001 });
  });

  it('gives the choice of port back to Grid', async () => {
    const { session } = open({
      ...ON,
      public: {
        ...OPEN_PUBLIC,
        port: 9000,
        port_picked: true,
        addresses: ['http://192.168.1.20:9000/']
      }
    });
    const pub = await card('public');
    await fireEvent.click(within(pub).getByRole('button', { name: 'Use the usual port' }));
    expect(session.putDoor).toHaveBeenCalledWith('Public', { enabled: true, port: 0 });
    await waitFor(() =>
      expect((within(pub).getByLabelText('Port') as HTMLInputElement).value).toBe('80')
    );
    // Nothing to give back when Grid already picks.
    expect(within(pub).queryByRole('button', { name: 'Use the usual port' })).toBeNull();
  });

  it('takes a low port such as 80, and refuses what cannot be a port before sending anything', async () => {
    const { session } = open(OFF);
    const remote = await card('remote-rd');
    const port = within(remote).getByLabelText('Port') as HTMLInputElement;
    for (const [typed, words] of [
      ['0', 'Pick a port from 1 to 65535.'],
      ['65536', 'Pick a port from 1 to 65535.'],
      ['abc', 'A port is a whole number.'],
      ['', 'Type a port number.']
    ] as const) {
      await fireEvent.input(port, { target: { value: typed } });
      expect(within(remote).getByRole('alert')).toHaveTextContent(words);
      expect(within(remote).getByRole('button', { name: 'Change port' })).toBeDisabled();
      // Enter in the box does not get round the button.
      await fireEvent.submit(screen.getByRole('form', { name: 'Remote RD port' }));
    }
    expect(session.putDoor).not.toHaveBeenCalled();
    // The same port as now is not a change.
    await fireEvent.input(port, { target: { value: '8082' } });
    expect(within(remote).queryByRole('alert')).toBeNull();
    expect(within(remote).queryByRole('button', { name: 'Change port' })).toBeNull();
    // Port 443 is a port like any other.
    await fireEvent.input(port, { target: { value: '443' } });
    await fireEvent.click(within(remote).getByRole('button', { name: 'Change port' }));
    expect(session.putDoor).toHaveBeenCalledWith('Remote', { enabled: false, port: 443 });
  });

  it('keeps what is being typed in a port box when the page reads the setting again', async () => {
    open(OFF);
    const port = within(await card('public')).getByLabelText('Port') as HTMLInputElement;
    await fireEvent.input(port, { target: { value: '90' } });
    await vi.advanceTimersByTimeAsync(11_000);
    expect(port.value).toBe('90');
  });

  it("shows the Director's words when a change is refused, and changes nothing", async () => {
    const { session } = open(OFF);
    session.putDoor.mockRejectedValueOnce(
      new Error('Port 8080 is the one Grid itself uses on this computer. Pick another.')
    );
    const pub = await card('public');
    const port = within(pub).getByLabelText('Port') as HTMLInputElement;
    await fireEvent.input(port, { target: { value: '8080' } });
    await fireEvent.click(within(pub).getByRole('button', { name: 'Change port' }));
    expect(await screen.findByText(/Port 8080 is the one Grid itself uses/)).toBeVisible();
    expect(toggle('Public')).not.toBeChecked();
    // What was typed is kept, to be mended.
    expect(port.value).toBe('8080');
    // The next try that works clears the message.
    await fireEvent.input(port, { target: { value: '9000' } });
    await fireEvent.click(within(pub).getByRole('button', { name: 'Change port' }));
    await waitFor(() => expect(screen.queryByText(/Port 8080 is the one/)).toBeNull());
  });

  it('a switch that the Director refuses goes back to where it was', async () => {
    const { session } = open(OFF);
    session.putDoor.mockRejectedValueOnce(new Error('The network setting could not be saved.'));
    await card('public');
    await fireEvent.click(toggle('Public'));
    expect(await screen.findByText(/could not be saved/)).toBeVisible();
    await waitFor(() => expect(toggle('Public')).not.toBeChecked());
  });

  it('a token prompt that is cancelled changes nothing', async () => {
    const { session } = open(OFF);
    session.putDoor.mockResolvedValueOnce(undefined);
    await card('public');
    await fireEvent.click(toggle('Public'));
    await waitFor(() => expect(session.putDoor).toHaveBeenCalledOnce());
    await waitFor(() => expect(toggle('Public')).not.toBeChecked());
    expect(toggle('Public')).toBeEnabled();
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
    await card('public');
  });

  it('notices a new address when the computer changes network', async () => {
    const { becomes } = open(ON);
    await screen.findByRole('link', { name: '192.168.1.20' });
    becomes({ ...ON, public: { ...OPEN_PUBLIC, addresses: ['http://10.42.0.1/'] } });
    await vi.advanceTimersByTimeAsync(5100);
    expect(await screen.findByRole('link', { name: '10.42.0.1' })).toBeVisible();
    expect(screen.queryByRole('link', { name: '192.168.1.20' })).toBeNull();
  });

  it('stops reading the setting when the page is left', async () => {
    const { session } = open(ON);
    await card('public');
    cleanup();
    const before = session.getNetwork.mock.calls.length;
    await vi.advanceTimersByTimeAsync(20_000);
    expect(session.getNetwork.mock.calls.length).toBe(before);
  });
});

describe('NetworkPage: the access token', () => {
  it('is shown on the Remote RD card, on the Grid computer', async () => {
    open(BOTH);
    const remote = await card('remote-rd');
    expect(within(remote).getByTestId('access-token')).toHaveTextContent(TOKEN);
    expect(within(await card('public')).queryByTestId('access-token')).toBeNull();
  });

  it('stays readable while the door is shut, so it can be handed out ahead', async () => {
    open({ ...ON, token: TOKEN });
    expect(await screen.findByTestId('access-token')).toHaveTextContent(TOKEN);
  });

  it('is not shown, and cannot be changed, on a device that signed in with it', async () => {
    // The Director leaves the token out of what it tells such a device.
    const { token: _hidden, ...told } = BOTH;
    open(told, { ...HELLO, caller: 'Director', door: 'Remote', director_sign_in: true });
    await card('remote-rd');
    await screen.findByRole('button', { name: 'Sign out on this device' });
    expect(screen.queryByTestId('access-token')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Change…' })).toBeNull();
    expect(document.body.textContent).not.toContain(TOKEN);
  });

  it('is copied', async () => {
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
    open(BOTH);
    await fireEvent.click(await screen.findByRole('button', { name: 'Copy the access token' }));
    expect(writeText).toHaveBeenCalledWith(TOKEN);
  });

  it('is changed to one Grid makes when nothing is typed, after saying who is signed out', async () => {
    const { session } = open(BOTH);
    await fireEvent.click(await screen.findByRole('button', { name: 'Change…' }));
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('Change the access token');
    expect(dialog).toHaveTextContent('Every device signed in with the old token is signed out.');
    // Nothing is sent by opening the dialog.
    expect(session.putAccessToken).not.toHaveBeenCalled();
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Change' }));
    await waitFor(() => expect(session.putAccessToken).toHaveBeenCalledWith(undefined));
    await waitFor(() =>
      expect(screen.getByTestId('access-token')).toHaveTextContent('NEW2-NEW3-NEW4')
    );
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('is changed to the one typed, tidied of the spaces around it', async () => {
    const { session } = open(BOTH);
    await fireEvent.click(await screen.findByRole('button', { name: 'Change…' }));
    const dialog = await screen.findByRole('dialog');
    await fireEvent.input(within(dialog).getByLabelText('New token'), {
      target: { value: '  sunday-race-7 ' }
    });
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Change' }));
    await waitFor(() => expect(session.putAccessToken).toHaveBeenCalledWith('sunday-race-7'));
    await waitFor(() =>
      expect(screen.getByTestId('access-token')).toHaveTextContent('sunday-race-7')
    );
  });

  it('refuses one that cannot be used before sending anything', async () => {
    const { session } = open(BOTH);
    await fireEvent.click(await screen.findByRole('button', { name: 'Change…' }));
    const dialog = await screen.findByRole('dialog');
    const box = within(dialog).getByLabelText('New token');
    for (const [typed, words] of [
      ['short', 'An access token needs at least 8 characters.'],
      ['two words here', 'An access token cannot have spaces in it.']
    ] as const) {
      await fireEvent.input(box, { target: { value: typed } });
      expect(within(dialog).getByRole('alert')).toHaveTextContent(words);
      expect(within(dialog).getByRole('button', { name: 'Change' })).toBeDisabled();
      await fireEvent.submit(within(dialog).getByRole('form', { name: 'Access token' }));
    }
    expect(session.putAccessToken).not.toHaveBeenCalled();
  });

  it('is left alone when the dialog is cancelled', async () => {
    const { session } = open(BOTH);
    await fireEvent.click(await screen.findByRole('button', { name: 'Change…' }));
    const dialog = await screen.findByRole('dialog');
    await fireEvent.input(within(dialog).getByLabelText('New token'), {
      target: { value: 'sunday-race-7' }
    });
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(session.putAccessToken).not.toHaveBeenCalled();
    expect(screen.getByTestId('access-token')).toHaveTextContent(TOKEN);
  });

  it("keeps the dialog open with the Director's words when the change is refused", async () => {
    const { session } = open(BOTH);
    session.putAccessToken.mockRejectedValueOnce(
      new Error('The access token can only be changed on the Grid computer.')
    );
    await fireEvent.click(await screen.findByRole('button', { name: 'Change…' }));
    const dialog = await screen.findByRole('dialog');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Change' }));
    expect(
      await within(dialog).findByText(/can only be changed on the Grid computer/)
    ).toBeVisible();
    expect(screen.getByTestId('access-token')).toHaveTextContent(TOKEN);
  });
});

describe('NetworkPage: this device', () => {
  it('offers sign-out only on a device signed in with the access token', async () => {
    open(ON);
    await card('remote-rd');
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

  it('gives the kiosk address from the Public door, and only when it is open', async () => {
    open({
      ...BOTH,
      public: { ...OPEN_PUBLIC, addresses: ['http://192.168.1.20/', 'http://10.42.0.1/'] }
    });
    const link = await screen.findByTestId('kiosk-link');
    expect(link).toHaveTextContent('192.168.1.20/#/kiosk');
    expect(link).toHaveAttribute('href', 'http://192.168.1.20/#/kiosk');
    cleanup();
    // The Remote RD door alone gives a kiosk no way in.
    open({ public: SHUT_PUBLIC, remote: OPEN_REMOTE, token: TOKEN });
    await card('public');
    expect(screen.queryByTestId('kiosk-link')).toBeNull();
    expect(screen.getByText('Enable Public first.')).toBeVisible();
    cleanup();
    // On, but this computer has no address to give.
    open({ ...ON, public: { ...OPEN_PUBLIC, addresses: [] } });
    await card('public');
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

  it('starts with no codes and no kiosks, and says in a few words what a kiosk is', async () => {
    open(ON);
    await screen.findByTestId('kiosk-link');
    const kiosksCard = screen.getByTestId('kiosks');
    expect(kiosksCard).toHaveTextContent('A tablet where pilots sign up');
    // No steps to read: the address, the button, and the codes when there are some.
    expect(kiosksCard.querySelector('ol')).toBeNull();
    expect(kiosksCard).not.toHaveTextContent('On the tablet, open');
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

  describe('naming a kiosk', () => {
    const NAMED: KioskView = {
      codes: [],
      kiosks: [
        { number: 1, set_up_at: 1_794_800_000 },
        { number: 2, set_up_at: 1_794_800_100, name: 'By the gate' }
      ]
    };
    const nameBox = () => screen.getByLabelText('Name') as HTMLInputElement;

    it('shows a name beside the kiosk that has one, as text whatever is in it', async () => {
      open(ON, HELLO, {
        codes: [],
        kiosks: [{ number: 1, set_up_at: 1, name: '<img src=x onerror=alert(1)> & co' }]
      });
      expect((await screen.findByTestId('kiosk-name')).textContent).toBe(
        '<img src=x onerror=alert(1)> & co'
      );
      expect(document.querySelector('img')).toBeNull();
    });

    it('names a kiosk that has none', async () => {
      const { session } = open(ON, HELLO, NAMED);
      await fireEvent.click(await screen.findByRole('button', { name: 'Name kiosk 1' }));
      const dialog = screen.getByRole('dialog');
      expect(within(dialog).getByRole('heading')).toHaveTextContent('Name kiosk 1');
      expect(nameBox().value).toBe('');
      await fireEvent.input(nameBox(), { target: { value: '  Pit lane  ' } });
      await fireEvent.submit(within(dialog).getByRole('form', { name: 'Kiosk name' }));
      await waitFor(() => expect(session.renameKiosk).toHaveBeenCalledWith(1, 'Pit lane'));
      await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
      expect(screen.getAllByTestId('kiosk-name').map((el) => el.textContent)).toEqual([
        'Pit lane',
        'By the gate'
      ]);
    });

    it('opens on the name it has, and an empty name takes it away', async () => {
      const { session } = open(ON, HELLO, NAMED);
      await fireEvent.click(await screen.findByRole('button', { name: 'Name kiosk 2' }));
      expect(nameBox().value).toBe('By the gate');
      expect(nameBox()).toHaveAttribute('maxlength', '40');
      await fireEvent.input(nameBox(), { target: { value: '   ' } });
      await fireEvent.submit(screen.getByRole('form', { name: 'Kiosk name' }));
      await waitFor(() => expect(session.renameKiosk).toHaveBeenCalledWith(2, ''));
      await waitFor(() => expect(screen.queryByTestId('kiosk-name')).toBeNull());
    });

    it('cancel sends nothing', async () => {
      const { session } = open(ON, HELLO, NAMED);
      await fireEvent.click(await screen.findByRole('button', { name: 'Name kiosk 1' }));
      await fireEvent.input(nameBox(), { target: { value: 'Never' } });
      await fireEvent.click(
        within(screen.getByRole('dialog')).getByRole('button', { name: 'Cancel' })
      );
      expect(screen.queryByRole('dialog')).toBeNull();
      expect(session.renameKiosk).not.toHaveBeenCalled();
    });

    it("stays open with the Director's reason when it refuses", async () => {
      const { session } = open(ON, HELLO, NAMED);
      session.renameKiosk.mockRejectedValueOnce(
        new Error('That kiosk is not on the list any more.')
      );
      await fireEvent.click(await screen.findByRole('button', { name: 'Name kiosk 1' }));
      await fireEvent.input(nameBox(), { target: { value: 'Gone' } });
      await fireEvent.submit(screen.getByRole('form', { name: 'Kiosk name' }));
      const dialog = screen.getByRole('dialog');
      expect(
        await within(dialog).findByText('That kiosk is not on the list any more.')
      ).toBeVisible();
      expect(nameBox().value).toBe('Gone');
    });
  });
});
