/**
 * A pilot's own profile on a remote server, and who may make an account there
 * (docs/primitives.md §13.7).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { MyAccount, PushClub, ServerEvent } from '@gridfpv/types';
import ServerHome from '../src/screens/ServerHome.svelte';
import {
  pilotName,
  profileChanged,
  profileForm,
  profileOf,
  profileProblem,
  signUpLine
} from '../src/lib/serverPeople.js';
import { eventView } from './competition.js';
import { ACE, NORTH, SOUTH, fakePeople, fakeServer, type FakeServer } from './serverPeopleFake.js';
import { fakeLiveSession } from './support.js';

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  location.hash = '';
});
afterEach(() => {
  cleanup();
  location.hash = '';
  vi.useRealTimers();
  vi.restoreAllMocks();
});

function open(hash: string, server: FakeServer = fakeServer()) {
  location.hash = hash;
  const people = fakePeople(server);
  const load = {
    clubs: vi.fn(async (): Promise<PushClub[]> => [NORTH, SOUTH]),
    events: vi.fn(async (): Promise<ServerEvent[]> => [])
  };
  render(ServerHome, {
    baseUrl: 'https://grid.example.org',
    load,
    people,
    sourceFor: () => fakeLiveSession(eventView({}), { subscribeChanges: vi.fn(() => vi.fn()) })
  });
  return { people, server };
}

const FULL = {
  callsign: 'AceOfSpades',
  color: '#FF8800',
  multigp_id: '12345',
  velocidrone_id: 'ace.fpv'
};
const OWNER: MyAccount = { ...ACE, id: 'acct_oz', name: 'Oz', owner: true, clubs: [] };

const card = () => screen.findByTestId('pilot-profile');
const box = (label: string) => screen.getByLabelText(label) as HTMLInputElement;
const save = () =>
  within(screen.getByRole('form', { name: 'Pilot profile' })).getByRole('button', {
    name: 'Save'
  }) as HTMLButtonElement;

describe('the words of a pilot profile', () => {
  it('turns a profile into a form and back, leaving out what is empty', () => {
    expect(profileForm(undefined)).toEqual({
      callsign: '',
      color: '',
      multigp_id: '',
      velocidrone_id: ''
    });
    expect(profileOf(profileForm(FULL))).toEqual(FULL);
    expect(
      profileOf({ callsign: '  Ace ', color: ' #ff8800 ', multigp_id: '  ', velocidrone_id: '' })
    ).toEqual({ callsign: 'Ace', color: '#FF8800' });
    expect(profileOf(profileForm({}))).toEqual({});
  });

  it('says what is wrong before it is sent', () => {
    const form = profileForm(FULL);
    expect(profileProblem(form)).toBeUndefined();
    expect(profileProblem(profileForm({}))).toBeUndefined();
    expect(profileProblem({ ...form, callsign: 'x'.repeat(41) })).toContain('callsign');
    expect(profileProblem({ ...form, callsign: 'é'.repeat(40) })).toBeUndefined();
    for (const bad of ['red', '#12345', '#1234567', 'FF8800', '#GGGGGG']) {
      expect(profileProblem({ ...form, color: bad }), bad).toContain('colour');
    }
    expect(profileProblem({ ...form, color: '#abcdef' })).toBeUndefined();
    expect(profileProblem({ ...form, multigp_id: '12 34' })).toBe(
      'A MultiGP id has no spaces in it.'
    );
    expect(profileProblem({ ...form, multigp_id: '1'.repeat(41) })).toContain('MultiGP');
    expect(profileProblem({ ...form, velocidrone_id: 'a b' })).toBe(
      'A Velocidrone id has no spaces in it.'
    );
    expect(profileProblem({ ...form, velocidrone_id: 'v'.repeat(41) })).toContain('Velocidrone');
  });

  it('knows whether anything was changed', () => {
    expect(profileChanged(profileForm(FULL), FULL)).toBe(false);
    // Spaces around a part, or a colour in small letters, are not a change.
    expect(profileChanged({ ...profileForm(FULL), callsign: ' AceOfSpades ' }, FULL)).toBe(false);
    expect(profileChanged({ ...profileForm(FULL), color: '#ff8800' }, FULL)).toBe(false);
    expect(profileChanged({ ...profileForm(FULL), callsign: 'Ace' }, FULL)).toBe(true);
    expect(profileChanged({ ...profileForm(FULL), multigp_id: '' }, FULL)).toBe(true);
    expect(profileChanged(profileForm({}), {})).toBe(false);
    expect(profileChanged(profileForm(undefined), undefined)).toBe(false);
  });

  it('calls a pilot by their callsign, or else by their account', () => {
    expect(pilotName({ name: 'Ace', profile: FULL })).toBe('AceOfSpades');
    expect(pilotName({ name: 'Ace', profile: {} })).toBe('Ace');
    expect(pilotName({ name: 'Ace', profile: { callsign: '  ' } })).toBe('Ace');
    expect(pilotName({ name: 'Ace' })).toBe('Ace');
  });

  it("says how an account is made, by the owner's choice", () => {
    expect(signUpLine(false)).toContain('need an invite');
    // Where anyone may sign up, the way in is already on the page.
    expect(signUpLine(true)).toBe('');
    // Not known yet: nothing is said that may turn out wrong.
    expect(signUpLine(undefined)).toBe('');
  });
});

describe('the pilot profile on the account page', () => {
  it('says nothing is filled in, and how the pilot is shown until it is', async () => {
    open('#/account', fakeServer({ account: ACE }));
    const profile = await card();
    expect(within(profile).getByTestId('profile-empty')).toHaveTextContent('Shown as Ace.');
    expect(within(profile).getByRole('button', { name: 'Fill it in' })).toBeTruthy();
    expect(screen.queryByLabelText('Callsign')).toBeNull();
  });

  it('shows what is filled in, and says what is not', async () => {
    open(
      '#/account',
      fakeServer({ account: { ...ACE, profile: { callsign: 'AceOfSpades', color: '#FF8800' } } })
    );
    const profile = await card();
    expect(within(profile).getByTestId('profile-callsign')).toHaveTextContent('AceOfSpades');
    expect(within(profile).getByTestId('profile-color')).toHaveTextContent('#FF8800');
    expect(within(profile).getByTestId('profile-multigp')).toHaveTextContent('Not set');
    expect(within(profile).getByTestId('profile-velocidrone')).toHaveTextContent('Not set');
    expect(within(profile).getByRole('button', { name: 'Change' })).toBeTruthy();
  });

  it('says the account name stands in when only an id is filled in', async () => {
    open('#/account', fakeServer({ account: { ...ACE, profile: { multigp_id: '777' } } }));
    const profile = await card();
    expect(within(profile).getByTestId('profile-callsign')).toHaveTextContent(
      'Not set. You are shown as Ace.'
    );
    expect(within(profile).getByTestId('profile-multigp')).toHaveTextContent('777');
  });

  it('fills the profile in, tidied, and shows it', async () => {
    const { people, server } = open('#/account', fakeServer({ account: ACE }));
    await fireEvent.click(within(await card()).getByRole('button', { name: 'Fill it in' }));
    // Nothing typed is nothing to save.
    expect(save().disabled).toBe(true);
    // The callsign box hints at the name used until one is given.
    expect(box('Callsign').placeholder).toBe('Ace');

    await fireEvent.input(box('Callsign'), { target: { value: '  AceOfSpades ' } });
    await fireEvent.input(box('Colour'), { target: { value: '#ff8800' } });
    await fireEvent.input(box('MultiGP id'), { target: { value: ' 12345 ' } });
    await fireEvent.input(box('Velocidrone id'), { target: { value: 'ace.fpv' } });
    expect(save().disabled).toBe(false);
    await fireEvent.click(save());
    await waitFor(() => expect(people.setProfile).toHaveBeenCalledWith(FULL));
    // The form closes and the profile is shown.
    await waitFor(() => expect(screen.queryByLabelText('Callsign')).toBeNull());
    expect(screen.getByTestId('profile-callsign')).toHaveTextContent('AceOfSpades');
    expect(screen.getByTestId('profile-velocidrone')).toHaveTextContent('ace.fpv');
    expect(server.account?.profile).toEqual(FULL);
    // The account's own name is another thing.
    expect(screen.getByTestId('account-name')).toHaveTextContent('Ace');
  });

  it('clears a part that is emptied', async () => {
    const { people } = open('#/account', fakeServer({ account: { ...ACE, profile: FULL } }));
    await fireEvent.click(within(await card()).getByRole('button', { name: 'Change' }));
    expect(box('Callsign').value).toBe('AceOfSpades');
    expect(box('MultiGP id').value).toBe('12345');
    // Nothing changed yet.
    expect(save().disabled).toBe(true);
    await fireEvent.input(box('MultiGP id'), { target: { value: '' } });
    await fireEvent.input(box('Colour'), { target: { value: '   ' } });
    await fireEvent.click(save());
    await waitFor(() =>
      expect(people.setProfile).toHaveBeenCalledWith({
        callsign: 'AceOfSpades',
        velocidrone_id: 'ace.fpv'
      })
    );
    await waitFor(() => expect(screen.getByTestId('profile-multigp')).toHaveTextContent('Not set'));
  });

  it('picks a colour with the colour box too', async () => {
    open('#/account', fakeServer({ account: ACE }));
    await fireEvent.click(within(await card()).getByRole('button', { name: 'Fill it in' }));
    await fireEvent.input(screen.getByLabelText('Pick a colour'), { target: { value: '#00ff7f' } });
    expect(box('Colour').value).toBe('#00FF7F');
    expect(save().disabled).toBe(false);
  });

  it('says what is wrong while typing, and does not send it', async () => {
    const { people } = open('#/account', fakeServer({ account: ACE }));
    await fireEvent.click(within(await card()).getByRole('button', { name: 'Fill it in' }));
    await fireEvent.input(box('Colour'), { target: { value: 'red' } });
    expect(screen.getByRole('alert')).toHaveTextContent('A colour is a # and six letters');
    expect(save().disabled).toBe(true);
    await fireEvent.input(box('Colour'), { target: { value: '#112233' } });
    await fireEvent.input(box('MultiGP id'), { target: { value: '12 34' } });
    expect(screen.getByRole('alert')).toHaveTextContent('A MultiGP id has no spaces in it.');
    expect(save().disabled).toBe(true);
    expect(people.setProfile).not.toHaveBeenCalled();
  });

  it("shows the server's own words when a save is refused, and keeps what was typed", async () => {
    const { people } = open('#/account', fakeServer({ account: ACE }));
    people.setProfile.mockRejectedValueOnce(new Error('A callsign is 40 characters at most.'));
    await fireEvent.click(within(await card()).getByRole('button', { name: 'Fill it in' }));
    await fireEvent.input(box('Callsign'), { target: { value: 'Ace' } });
    await fireEvent.click(save());
    expect(await screen.findByTestId('profile-error')).toHaveTextContent(
      'A callsign is 40 characters at most.'
    );
    expect(box('Callsign').value).toBe('Ace');
    // The refusal goes once the pilot types again.
    await fireEvent.input(box('Callsign'), { target: { value: 'Ace2' } });
    expect(screen.queryByTestId('profile-error')).toBeNull();
  });

  it('cancelling saves nothing, and the form opens again on what is kept', async () => {
    const { people } = open('#/account', fakeServer({ account: { ...ACE, profile: FULL } }));
    await fireEvent.click(within(await card()).getByRole('button', { name: 'Change' }));
    await fireEvent.input(box('Callsign'), { target: { value: 'Other' } });
    await fireEvent.click(
      within(screen.getByRole('form', { name: 'Pilot profile' })).getByRole('button', {
        name: 'Cancel'
      })
    );
    expect(people.setProfile).not.toHaveBeenCalled();
    expect(screen.getByTestId('profile-callsign')).toHaveTextContent('AceOfSpades');
    await fireEvent.click(within(await card()).getByRole('button', { name: 'Change' }));
    expect(box('Callsign').value).toBe('AceOfSpades');
  });

  it('works for an account from a server that does not send a profile', async () => {
    // An older server's answer has no profile at all.
    const { profile: _profile, ...old } = ACE;
    open('#/account', fakeServer({ account: old as MyAccount }));
    expect(within(await card()).getByTestId('profile-empty')).toBeTruthy();
  });
});

/** The switch on the owner's card for who may make an account. */
const flip = (block: HTMLElement) =>
  within(block).getByRole('switch', { name: 'Let anyone make an account' }) as HTMLInputElement;

describe('who may make an account', () => {
  const setting = () => screen.findByTestId('sign-up-setting');

  it('is not shown to someone who is not an owner', async () => {
    const { people } = open('#/admin/server/sign-in', fakeServer({ account: ACE }));
    await screen.findByTestId('server-who');
    expect(screen.queryByTestId('sign-up-setting')).toBeNull();
    expect(people.setOpenSignUp).not.toHaveBeenCalled();
  });

  it('starts as invite only, and an owner lets anyone in after being asked', async () => {
    const { people, server } = open('#/admin/server/sign-in', fakeServer({ account: OWNER }));
    const block = await setting();
    await waitFor(() =>
      expect(within(block).getByTestId('sign-up-state')).toHaveTextContent('Invite only')
    );
    expect(block).toHaveTextContent('Invites work either way.');

    await fireEvent.click(flip(block));
    const dialog = screen.getByRole('dialog');
    expect(dialog).toHaveTextContent('Let anyone make an account?');
    expect(dialog).toHaveTextContent('A new account is in no club.');
    expect(people.setOpenSignUp).not.toHaveBeenCalled();

    // Cancel does nothing.
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(people.setOpenSignUp).not.toHaveBeenCalled();
    expect(server.openSignUp).toBe(false);

    await fireEvent.click(flip(block));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Let anyone in' })
    );
    await waitFor(() => expect(people.setOpenSignUp).toHaveBeenCalledWith(true));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(within(block).getByTestId('sign-up-state')).toHaveTextContent('Anyone');
    expect(flip(block).checked).toBe(true);
    expect(server.openSignUp).toBe(true);
  });

  it('goes back to invites at once, with no question', async () => {
    const { people, server } = open(
      '#/admin/server/sign-in',
      fakeServer({ account: OWNER, openSignUp: true })
    );
    const block = await setting();
    await waitFor(() =>
      expect(within(block).getByTestId('sign-up-state')).toHaveTextContent('Anyone')
    );
    await fireEvent.click(flip(block));
    await waitFor(() => expect(people.setOpenSignUp).toHaveBeenCalledWith(false));
    expect(screen.queryByRole('dialog')).toBeNull();
    await waitFor(() =>
      expect(within(block).getByTestId('sign-up-state')).toHaveTextContent('Invite only')
    );
    expect(server.openSignUp).toBe(false);
  });

  it('says why when the server refuses, and changes nothing', async () => {
    const { people } = open('#/admin/server/sign-in', fakeServer({ account: OWNER }));
    const block = await setting();
    people.setOpenSignUp.mockRejectedValueOnce(new Error('The server could not save that.'));
    await waitFor(() => expect(within(block).queryByRole('switch')).not.toBeNull());
    await fireEvent.click(flip(block));
    const dialog = screen.getByRole('dialog');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Let anyone in' }));
    expect(await within(dialog).findByText('The server could not save that.')).toBeTruthy();
    expect(within(block).getByTestId('sign-up-state')).toHaveTextContent('Invite only');
  });

  it('says so when the setting cannot be read', async () => {
    const server = fakeServer({ account: OWNER });
    location.hash = '#/admin/server/sign-in';
    const people = fakePeople(server);
    people.serverInfo.mockRejectedValue(new Error('The server is not answering.'));
    render(ServerHome, {
      baseUrl: 'https://grid.example.org',
      load: {
        clubs: vi.fn(async (): Promise<PushClub[]> => [NORTH]),
        events: vi.fn(async (): Promise<ServerEvent[]> => [])
      },
      people,
      sourceFor: () => fakeLiveSession(eventView({}), { subscribeChanges: vi.fn(() => vi.fn()) })
    });
    const block = await setting();
    expect(await within(block).findByText('The server is not answering.')).toBeTruthy();
    // Neither choice is offered while it is not known which holds.
    expect(within(block).queryByRole('switch')).toBeNull();
  });
});

describe('the sign-in page', () => {
  it('says an invite is needed on a server that asks for one', async () => {
    open('#/sign-in');
    await waitFor(() =>
      expect(screen.getByTestId('sign-up-line')).toHaveTextContent('New here? You need an invite.')
    );
  });

  it('says nothing of invites on a server that lets anyone in', async () => {
    open('#/sign-in', fakeServer({ openSignUp: true }));
    // The sign-in itself is the same: no invite goes along.
    const link = await screen.findByRole('link', { name: 'Continue with Discord' });
    expect(link.getAttribute('href')).not.toContain('invite');
    expect(screen.queryByTestId('sign-up-line')).toBeNull();
  });

  it('follows the owner changing it, without the page being opened again', async () => {
    open('#/admin/server/sign-in', fakeServer({ account: OWNER, openSignUp: true }));
    const block = await screen.findByTestId('sign-up-setting');
    await waitFor(() => expect(flip(block).checked).toBe(true));
    await fireEvent.click(flip(block));
    await waitFor(() =>
      expect(within(block).getByTestId('sign-up-state')).toHaveTextContent('Invite only')
    );
  });
});
