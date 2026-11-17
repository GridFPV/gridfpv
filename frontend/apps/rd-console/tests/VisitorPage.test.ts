/**
 * The page someone on another device sees, and the director's sign-in on it.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import type { PublicHello } from '@gridfpv/types';
import VisitorPage from '../src/screens/VisitorPage.svelte';

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const HELLO: PublicHello = {
  version: '0.5.1',
  caller: 'Visitor',
  event: 'Friday Night Whoops',
  director_sign_in: true
};

function open(hello: PublicHello = HELLO) {
  const session = { signInDirector: vi.fn(async (_token: string) => {}) };
  const reload = vi.fn();
  render(VisitorPage, { session, hello, reload });
  return { session, reload };
}

const askToSignIn = () =>
  fireEvent.click(screen.getByRole('button', { name: 'I am the director' }));
const tokenBox = () => screen.getByLabelText('Access token') as HTMLInputElement;

describe('VisitorPage', () => {
  it('names the open event and says the visitor can only look', () => {
    open();
    expect(screen.getByTestId('visitor-event')).toHaveTextContent('Friday Night Whoops');
    expect(screen.getByText(/You can look\./)).toBeVisible();
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

  it('has no director sign-in when Grid has no access token set', () => {
    open({ ...HELLO, director_sign_in: false });
    expect(screen.queryByRole('button', { name: 'I am the director' })).toBeNull();
    expect(screen.queryByLabelText('Access token')).toBeNull();
  });

  it('keeps the token box away until it is asked for, and hides what is typed', async () => {
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

  it('cancel puts the sign-in away and forgets the error', async () => {
    const { session } = open();
    session.signInDirector.mockRejectedValueOnce(new Error('No.'));
    await askToSignIn();
    await fireEvent.input(tokenBox(), { target: { value: 'nope' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Sign in' }));
    await screen.findByRole('alert');
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByLabelText('Access token')).toBeNull();
    await askToSignIn();
    expect(screen.queryByRole('alert')).toBeNull();
  });
});
