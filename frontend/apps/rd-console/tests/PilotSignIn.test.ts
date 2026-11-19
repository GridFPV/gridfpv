/**
 * The director's card of pilot sign-in codes.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { PilotAccessView, PilotCodeView } from '@gridfpv/types';
import { buildNames } from '../src/lib/names.js';
import PilotSignIn from '../src/screens/PilotSignIn.svelte';

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const NAMES = buildNames(
  [],
  [
    { id: 'p-bee', callsign: 'Bee', vtx_types: [] },
    { id: 'p-ace', callsign: 'Ace', vtx_types: [] }
  ]
);
const ACCESS: PilotAccessView = {
  pilots: [
    { pilot: 'p-bee', code: 'XYZ789', signed_in: true },
    { pilot: 'p-ace', code: 'ABC234', signed_in: false }
  ],
  address: 'http://192.168.1.20:8081/'
};

function open(first: PilotAccessView = ACCESS, cursor = 5) {
  let now = first;
  const session = {
    getPilotAccess: vi.fn(async (_event: string) => now),
    resetPilotCode: vi.fn(
      async (_event: string, pilot: string): Promise<PilotCodeView | undefined> => ({
        pilot,
        code: 'NEW456',
        signed_in: false
      })
    ),
    endPilotSession: vi.fn(
      async (_event: string, _pilot: string): Promise<true | undefined> => true
    ),
    pilotQrUrl: (event: string, pilot: string) => `/events/${event}/access/${pilot}/qr`
  };
  const view = render(PilotSignIn, { session, event: 'evt-night', names: NAMES, cursor });
  return { session, view, becomes: (next: PilotAccessView) => (now = next) };
}

const show = () => fireEvent.click(screen.getByRole('button', { name: 'Show codes' }));
const table = () => screen.findByRole('table', { name: 'Pilot sign-in codes' });

describe('PilotSignIn', () => {
  it('shows no code, and asks the Director for none, until the director opens it', async () => {
    const { session } = open();
    expect(screen.queryByRole('table')).toBeNull();
    expect(document.body.textContent).not.toContain('ABC');
    await vi.advanceTimersByTimeAsync(12_000);
    expect(session.getPilotAccess).not.toHaveBeenCalled();
  });

  it('lists every pilot by callsign with their code, and who is signed in', async () => {
    open();
    await show();
    const rows = within(await table())
      .getAllByRole('row')
      .slice(1);
    expect(rows.map((row) => within(row).getByRole('rowheader').textContent)).toEqual([
      'Ace',
      'Bee'
    ]);
    expect(within(rows[0]).getByTestId('pilot-code')).toHaveTextContent('ABC 234');
    expect(within(rows[1]).getByTestId('pilot-code')).toHaveTextContent('XYZ 789');
    expect(rows[0]).not.toHaveTextContent('Signed in');
    expect(rows[1]).toHaveTextContent('Signed in');
    // Sign out is offered only for someone who is signed in.
    expect(within(rows[0]).queryByRole('button', { name: 'Sign Ace out' })).toBeNull();
    expect(within(rows[1]).getByRole('button', { name: 'Sign Bee out' })).toBeEnabled();
    // No raw id is on the page.
    expect(document.body.textContent).not.toContain('p-ace');
  });

  it('hides the codes again, and stops asking', async () => {
    const { session } = open();
    await show();
    await table();
    await fireEvent.click(screen.getByRole('button', { name: 'Hide codes' }));
    expect(screen.queryByRole('table')).toBeNull();
    const asked = session.getPilotAccess.mock.calls.length;
    await vi.advanceTimersByTimeAsync(12_000);
    expect(session.getPilotAccess.mock.calls.length).toBe(asked);
  });

  it('notices by itself when a pilot signs in', async () => {
    const { becomes } = open();
    await show();
    const ace = () => screen.getByRole('row', { name: /Ace/ });
    await table();
    expect(ace()).not.toHaveTextContent('Signed in');
    becomes({
      ...ACCESS,
      pilots: ACCESS.pilots.map((entry) => ({ ...entry, signed_in: true }))
    });
    await vi.advanceTimersByTimeAsync(5100);
    await waitFor(() => expect(ace()).toHaveTextContent('Signed in'));
  });

  it('reads the codes again when the event changes, so a pilot who joined is there', async () => {
    const { session, view, becomes } = open();
    await show();
    await table();
    const before = session.getPilotAccess.mock.calls.length;
    becomes({
      ...ACCESS,
      pilots: [...ACCESS.pilots, { pilot: 'p-new', code: 'JJJ222', signed_in: false }]
    });
    await view.rerender({ cursor: 6 });
    await waitFor(() => expect(session.getPilotAccess.mock.calls.length).toBe(before + 1));
    // A pilot the name list does not have yet is not shown by id.
    expect(await screen.findByText('JJJ 222')).toBeVisible();
    expect(document.body.textContent).not.toContain('p-new');
  });

  it('shows a pilot their QR code, with the address and the code to type instead', async () => {
    open();
    await show();
    await fireEvent.click(await screen.findByRole('button', { name: 'Show the QR code for Ace' }));
    const dialog = screen.getByRole('dialog');
    expect(within(dialog).getByRole('heading')).toHaveTextContent('Sign in: Ace');
    const picture = within(dialog).getByRole('img', { name: 'QR code that signs Ace in' });
    expect(picture).toHaveAttribute('src', '/events/evt-night/access/p-ace/qr');
    expect(dialog).toHaveTextContent('http://192.168.1.20:8081/');
    expect(within(dialog).getByTestId('qr-code-text')).toHaveTextContent('ABC 234');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Done' }));
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('with the network off, says so and offers no QR code, but still shows the codes', async () => {
    const { address: _none, ...noAddress } = ACCESS;
    open(noAddress);
    await show();
    await table();
    expect(screen.getByText(/Phones cannot reach Grid yet/)).toBeVisible();
    expect(screen.getByRole('link', { name: 'Network page' })).toHaveAttribute('href', '#/network');
    expect(screen.getByRole('button', { name: 'Show the QR code for Ace' })).toBeDisabled();
    expect(screen.getAllByTestId('pilot-code')[0]).toHaveTextContent('ABC 234');
  });

  it('asks before giving a new code, and sends nothing when the answer is no', async () => {
    const { session } = open();
    await show();
    await fireEvent.click(await screen.findByRole('button', { name: 'Give Bee a new code' }));
    const dialog = screen.getByRole('dialog');
    expect(dialog).toHaveTextContent('The code Bee has now stops working.');
    // Bee is signed in, so it says what happens to the phone.
    expect(dialog).toHaveTextContent('Their phone is signed out.');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(session.resetPilotCode).not.toHaveBeenCalled();
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('gives a new code, shows it at once, and marks the pilot signed out', async () => {
    const { session } = open();
    await show();
    await fireEvent.click(await screen.findByRole('button', { name: 'Give Bee a new code' }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'New code' })
    );
    await waitFor(() => expect(session.resetPilotCode).toHaveBeenCalledWith('evt-night', 'p-bee'));
    const bee = await screen.findByRole('row', { name: /Bee/ });
    await waitFor(() => expect(within(bee).getByTestId('pilot-code')).toHaveTextContent('NEW 456'));
    expect(bee).not.toHaveTextContent('Signed in');
    // The other pilot's code is as it was.
    expect(
      within(screen.getByRole('row', { name: /Ace/ })).getByTestId('pilot-code')
    ).toHaveTextContent('ABC 234');
  });

  it('does not say a phone is signed out for a pilot who is not signed in', async () => {
    open();
    await show();
    await fireEvent.click(await screen.findByRole('button', { name: 'Give Ace a new code' }));
    expect(screen.getByRole('dialog')).not.toHaveTextContent('Their phone is signed out.');
  });

  it("signs a pilot's phone out and reads the list again", async () => {
    const { session, becomes } = open();
    await show();
    const out = await screen.findByRole('button', { name: 'Sign Bee out' });
    becomes({ ...ACCESS, pilots: ACCESS.pilots.map((entry) => ({ ...entry, signed_in: false })) });
    await fireEvent.click(out);
    await waitFor(() => expect(session.endPilotSession).toHaveBeenCalledWith('evt-night', 'p-bee'));
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Sign Bee out' })).toBeNull());
  });

  it('a cancelled token prompt changes nothing', async () => {
    const { session } = open();
    session.resetPilotCode.mockResolvedValueOnce(undefined);
    session.endPilotSession.mockResolvedValueOnce(undefined);
    await show();
    await fireEvent.click(await screen.findByRole('button', { name: 'Give Bee a new code' }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'New code' })
    );
    await waitFor(() => expect(session.resetPilotCode).toHaveBeenCalledOnce());
    expect(
      within(screen.getByRole('row', { name: /Bee/ })).getByTestId('pilot-code')
    ).toHaveTextContent('XYZ 789');
    const asked = session.getPilotAccess.mock.calls.length;
    await fireEvent.click(screen.getByRole('button', { name: 'Sign Bee out' }));
    await waitFor(() => expect(session.endPilotSession).toHaveBeenCalledOnce());
    expect(session.getPilotAccess.mock.calls.length).toBe(asked);
  });

  it("shows the Director's words when a change is refused", async () => {
    const { session } = open();
    session.resetPilotCode.mockRejectedValueOnce(
      new Error('That pilot is not in this event, so they have no sign-in code.')
    );
    await show();
    await fireEvent.click(await screen.findByRole('button', { name: 'Give Ace a new code' }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'New code' })
    );
    expect(await screen.findByText(/not in this event/)).toBeVisible();
  });

  it('says so when the codes cannot be read, and recovers', async () => {
    const { session } = open();
    session.getPilotAccess.mockRejectedValueOnce(new Error('Could not reach Grid.'));
    await show();
    expect(await screen.findByText('The codes could not be read.')).toBeVisible();
    await vi.advanceTimersByTimeAsync(5100);
    expect(await table()).toBeVisible();
    expect(screen.queryByText('The codes could not be read.')).toBeNull();
  });

  it('says so when no pilot is in the event', async () => {
    open({ pilots: [], address: 'http://192.168.1.20:8081/' });
    await show();
    expect(await screen.findByTestId('no-pilots')).toHaveTextContent(
      'No pilot is in this event yet.'
    );
  });
});

describe('PilotSignIn: the printed sheet', () => {
  const MANY: PilotAccessView = {
    address: 'http://192.168.1.20/',
    pilots: Array.from({ length: 11 }, (_, n) => ({
      pilot: n === 0 ? 'p-ace' : n === 1 ? 'p-bee' : `p-x${n}`,
      code: `AAA${String(200 + n)}`,
      signed_in: false
    }))
  };
  /** Press Print, and answer what the sheet held at the moment the print box opened. */
  async function printed(access: PilotAccessView, perPage?: '4' | '6') {
    const opened = open(access);
    let sheet:
      | { pages: number; cards: string[]; perPage: string | undefined; src: string[] }
      | undefined;
    const print = vi.spyOn(window, 'print').mockImplementation(() => {
      const el = screen.getByTestId('code-sheet');
      sheet = {
        pages: within(el).getAllByRole('region').length,
        cards: within(el)
          .getAllByRole('article')
          .map((card) => (card.textContent ?? '').replace(/\s+/g, ' ').trim()),
        perPage: el.dataset.perPage,
        src: [...el.querySelectorAll('img')].map((img) => img.getAttribute('src') ?? '')
      };
    });
    if (perPage) {
      await fireEvent.change(screen.getByLabelText('Cards on a page'), {
        target: { value: perPage }
      });
    }
    await fireEvent.click(screen.getByRole('button', { name: 'Print' }));
    // The QR pictures never load in a test: say they have, as a browser would. The page
    // starts waiting for them a moment after the sheet appears, so keep saying it.
    await waitFor(() => {
      for (const img of screen.queryByTestId('code-sheet')?.querySelectorAll('img') ?? []) {
        void fireEvent.load(img);
      }
      expect(print).toHaveBeenCalledOnce();
    });
    return { ...opened, sheet: sheet!, print };
  }

  it('prints a card for every pilot, by callsign, with their code and a QR code of their own', async () => {
    const { sheet, session } = await printed(ACCESS);
    expect(sheet.cards).toHaveLength(2);
    // In the same order as the table: by callsign.
    expect(sheet.cards[0]).toContain('Ace');
    expect(sheet.cards[0]).toContain('ABC 234');
    expect(sheet.cards[1]).toContain('Bee');
    expect(sheet.cards[1]).toContain('XYZ 789');
    expect(sheet.cards[0]).toContain('open http://192.168.1.20:8081/ and type the code');
    expect(sheet.src).toEqual([
      '/events/evt-night/access/p-ace/qr',
      '/events/evt-night/access/p-bee/qr'
    ]);
    // No id on paper.
    expect(sheet.cards.join(' ')).not.toMatch(/p-ace|p-bee|evt-night/);
    // It read the codes fresh, without the card having been opened.
    expect(session.getPilotAccess).toHaveBeenCalled();
  });

  it('puts six to a page unless four is picked', async () => {
    // Nine did not fit a page, so it is not offered.
    open(ACCESS);
    expect(
      within(screen.getByLabelText('Cards on a page'))
        .getAllByRole('option')
        .map((option) => option.textContent?.trim())
    ).toEqual(['4 to a page', '6 to a page']);
    cleanup();
    const six = await printed(MANY);
    expect(six.sheet.perPage).toBe('6');
    expect(six.sheet.cards).toHaveLength(11);
    expect(six.sheet.pages).toBe(2);
    cleanup();
    vi.restoreAllMocks();
    const four = await printed(MANY, '4');
    expect(four.sheet.perPage).toBe('4');
    expect(four.sheet.cards).toHaveLength(11);
    expect(four.sheet.pages).toBe(3);
  });

  it('leaves no codes in the page once the print box is closed, and none before it was pressed', async () => {
    open(ACCESS);
    expect(screen.queryByTestId('code-sheet')).toBeNull();
    expect(document.body.textContent).not.toContain('ABC 234');
    cleanup();
    await printed(ACCESS);
    await waitFor(() => expect(screen.queryByTestId('code-sheet')).toBeNull());
    expect(document.body.textContent).not.toContain('ABC 234');
  });

  it('does not print with no address for the QR codes, and says what to switch on', async () => {
    open({ ...ACCESS, address: undefined });
    const print = vi.spyOn(window, 'print').mockImplementation(() => {});
    await fireEvent.click(screen.getByRole('button', { name: 'Print' }));
    expect(await screen.findByText(/Switch on Public, on the Network page, first/)).toBeVisible();
    expect(print).not.toHaveBeenCalled();
    expect(screen.queryByTestId('code-sheet')).toBeNull();
  });

  it('does not print an empty sheet', async () => {
    open({ pilots: [], address: 'http://192.168.1.20/' });
    const print = vi.spyOn(window, 'print').mockImplementation(() => {});
    await fireEvent.click(screen.getByRole('button', { name: 'Print' }));
    expect(await screen.findByText('No pilot is in this event yet.')).toBeVisible();
    expect(print).not.toHaveBeenCalled();
  });
});
