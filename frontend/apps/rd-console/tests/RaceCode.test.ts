/**
 * The director's race code card: the one code pilots type on a server's page to show they
 * are at the race (docs/primitives.md §13.13).
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { RaceCodeView } from '@gridfpv/types';
import RaceCode from '../src/screens/RaceCode.svelte';

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const LINK = 'https://grid.example.org/#/here/club/friday/ABC234';

function fake(start: RaceCodeView = { code: 'ABC234', link: LINK }) {
  let view = start;
  const session = {
    getRaceCode: vi.fn(async (): Promise<RaceCodeView> => view),
    newRaceCode: vi.fn(async (): Promise<RaceCodeView | undefined> => {
      view = { ...view, code: 'XYZ789' };
      return view;
    }),
    raceCodeQrUrl: vi.fn((event: string) => `http://director/events/${event}/race-code/qr`)
  };
  render(RaceCode, { session, event: 'friday' });
  return session;
}

const show = () => fireEvent.click(screen.getByRole('button', { name: 'Show' }));
const code = () => screen.findByTestId('race-code-text');

describe('the race code card', () => {
  it('is closed until the director opens it, and reads nothing before then', async () => {
    const session = fake();
    expect(screen.queryByTestId('race-code-text')).toBeNull();
    expect(session.getRaceCode).not.toHaveBeenCalled();
    expect(document.body.textContent).not.toContain('ABC');

    await show();
    expect(await code()).toHaveTextContent('ABC 234');
    expect(session.getRaceCode).toHaveBeenCalledWith('friday');

    await fireEvent.click(screen.getByRole('button', { name: 'Hide' }));
    expect(screen.queryByTestId('race-code-text')).toBeNull();
  });

  it('shows a QR code to scan when a server is linked', async () => {
    const session = fake();
    await show();
    await code();
    expect(screen.queryByTestId('race-code-no-link')).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Show the QR code' }));
    const dialog = screen.getByRole('dialog');
    const picture = within(dialog).getByRole('img') as HTMLImageElement;
    expect(picture.getAttribute('src')).toBe('http://director/events/friday/race-code/qr');
    expect(picture.alt).toContain('race code');
    expect(dialog.textContent).toContain('ABC 234');
    expect(session.raceCodeQrUrl).toHaveBeenCalledWith('friday');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Done' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  });

  it('says how to get a QR code when no server is linked, and still shows the code', async () => {
    fake({ code: 'ABC234' });
    await show();
    expect(await code()).toHaveTextContent('ABC 234');
    expect(screen.getByTestId('race-code-no-link').textContent).toContain('Link a server');
    expect(screen.queryByRole('button', { name: 'Show the QR code' })).toBeNull();
  });

  it('makes a new code only after asking, in a dialog of the page', async () => {
    const confirm = vi.spyOn(window, 'confirm');
    const session = fake();
    await show();
    await code();
    await fireEvent.click(screen.getByRole('button', { name: 'New code…' }));
    const dialog = screen.getByRole('dialog');
    expect(dialog.textContent).toContain('Make a new race code?');
    expect(dialog.textContent).toContain('old code stops working');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(session.newRaceCode).not.toHaveBeenCalled();
    expect(await code()).toHaveTextContent('ABC 234');

    await fireEvent.click(screen.getByRole('button', { name: 'New code…' }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'New code' })
    );
    await waitFor(() => expect(session.newRaceCode).toHaveBeenCalledWith('friday'));
    await waitFor(() => expect(screen.getByTestId('race-code-text')).toHaveTextContent('XYZ 789'));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(confirm).not.toHaveBeenCalled();
  });

  it('keeps the code when the director backs out of the sign-in prompt', async () => {
    const session = fake();
    session.newRaceCode.mockResolvedValueOnce(undefined);
    await show();
    await code();
    await fireEvent.click(screen.getByRole('button', { name: 'New code…' }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'New code' })
    );
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(screen.getByTestId('race-code-text')).toHaveTextContent('ABC 234');
  });

  it("shows Grid's words when the code cannot be read or changed", async () => {
    const session = fake();
    session.getRaceCode.mockRejectedValueOnce(new Error('Grid could not read the race code.'));
    await show();
    expect(await screen.findByText('Grid could not read the race code.')).toBeTruthy();
    expect(screen.queryByTestId('race-code-text')).toBeNull();

    // Opened again, it reads again.
    await fireEvent.click(screen.getByRole('button', { name: 'Hide' }));
    await show();
    await code();
    session.newRaceCode.mockRejectedValueOnce(new Error('Only the director can do that.'));
    await fireEvent.click(screen.getByRole('button', { name: 'New code…' }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'New code' })
    );
    expect(await screen.findByText('Only the director can do that.')).toBeTruthy();
    expect(screen.getByTestId('race-code-text')).toHaveTextContent('ABC 234');
  });
});
