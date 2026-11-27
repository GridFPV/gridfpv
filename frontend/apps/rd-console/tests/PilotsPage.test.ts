import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/svelte';
import { waitFor } from '@testing-library/dom';
import type { LinkPeople, LinkView, Pilot, PullRequest } from '@gridfpv/types';
import PilotsPage from '../src/screens/PilotsPage.svelte';
import { makeTestSession } from './support.js';

afterEach(() => cleanup());

const PILOTS: Pilot[] = [
  { id: 'p1', callsign: 'Ace', name: 'Alice', country: 'US', vtx_types: [] },
  { id: 'p2', callsign: 'Bee', vtx_types: [] }
];

describe('PilotsPage (#74) — hosts the shared PilotManager', () => {
  it('lists the directory and exposes Add / Edit / Remove controls', async () => {
    const listPilotsImpl = vi.fn(async () => PILOTS);
    const { session } = makeTestSession({ noEnter: true, listPilotsImpl });
    render(PilotsPage, { session });

    await screen.findByText('Ace');
    expect(screen.getByText('Bee')).toBeInTheDocument();
    // The page's primary action opens the add form.
    expect(screen.getByRole('button', { name: '+ Add pilot' })).toBeInTheDocument();
    // Per-row management controls exist.
    expect(screen.getAllByRole('button', { name: 'Edit' })).toHaveLength(2);
    expect(screen.getAllByRole('button', { name: 'Remove' })).toHaveLength(2);
  });

  it('surfaces a read error with a retry', async () => {
    const listPilotsImpl = vi.fn(async () => {
      throw new Error('GET /pilots failed: HTTP 503');
    });
    const { session } = makeTestSession({ noEnter: true, listPilotsImpl });
    render(PilotsPage, { session });

    await waitFor(() => expect(screen.getByText(/HTTP 503/)).toBeInTheDocument());
    expect(screen.getByRole('button', { name: 'Try again' })).toBeInTheDocument();
  });

  it('has its help beside the title, and no tab row: Classes is its own page under Setup', () => {
    const { session } = makeTestSession({ noEnter: true });
    render(PilotsPage, { session });
    expect(screen.queryByRole('navigation')).toBeNull();
    expect(screen.getByRole('link', { name: 'Help for Pilots' })).toHaveAttribute(
      'href',
      '#/help/pilots'
    );
  });
});

// ── With a server linked (docs/primitives.md §13.7) ─────────────────────────

const PIA = { id: 'acct_pia', name: 'Pia' };
const QUIN = { id: 'acct_quin', name: 'Quin' };

function linkView(state: LinkView['state']): LinkView {
  return {
    clubs: [{ id: 'k1', name: '', url: 'https://grid.example.org', trust: 'System' }],
    open: 'friday',
    // The open event goes to the club, unless nothing is set.
    ...(state === 'NotSet' ? {} : { club: 'k1' }),
    state,
    events: []
  };
}

/** A Director with Ace and Bee, linked to a server whose club has Pia and Quin. */
function linked(over: Partial<LinkPeople> = {}, state: LinkView['state'] = 'Connected') {
  const server = {
    pilots: [...PILOTS] as Pilot[],
    people: {
      members: [PIA, QUIN],
      pilots: [
        { id: 'p1', callsign: 'Ace' },
        { id: 'p2', callsign: 'Bee' }
      ],
      ...over
    } as LinkPeople
  };
  const seams = {
    listPilotsImpl: vi.fn(async () => [...server.pilots]),
    getServerLinkImpl: vi.fn(async () => linkView(state)),
    getServerPeopleImpl: vi.fn(async () => server.people),
    setPilotServerAccountImpl: vi.fn(async (_base: string, id: string, account?: string) => {
      const member = server.people.members.find((m) => m.id === account);
      server.pilots = server.pilots.map((p) => {
        if (p.id !== id) return p;
        const { server_account: _was, ...rest } = p;
        return member ? { ...rest, server_account: member } : rest;
      });
      return server.pilots.find((p) => p.id === id)!;
    }),
    pullFromServerImpl: vi.fn(async (_base: string, request: PullRequest) => {
      const added: Pilot[] = [
        ...request.pilots.map((id) => {
          const theirs = server.people.pilots.find((p) => p.id === id)!;
          return {
            id,
            callsign: theirs.callsign,
            vtx_types: [],
            ...(theirs.account ? { server_account: theirs.account } : {})
          };
        }),
        ...request.members.map((id) => {
          const member = server.people.members.find((m) => m.id === id)!;
          return { id: `new-${id}`, callsign: member.name, vtx_types: [], server_account: member };
        })
      ];
      server.pilots = [...server.pilots, ...added];
      return added;
    })
  };
  const { session } = makeTestSession({ noEnter: true, ...seams });
  render(PilotsPage, { session });
  return { server, ...seams };
}

describe('PilotsPage with no server linked', () => {
  it('offers nothing that needs a server', async () => {
    // The open event goes to no club.
    const { getServerPeopleImpl } = linked({}, 'NotSet');
    await screen.findByText('Ace');
    expect(screen.queryByRole('button', { name: 'Add from the server' })).toBeNull();
    expect(screen.queryByRole('button', { name: /^Account of/ })).toBeNull();
    expect(getServerPeopleImpl).not.toHaveBeenCalled();
  });

  it('works as it always has when the link cannot be read', async () => {
    const { session } = makeTestSession({
      noEnter: true,
      listPilotsImpl: vi.fn(async () => PILOTS),
      getServerLinkImpl: vi.fn(async () => {
        throw new Error('nope');
      })
    });
    render(PilotsPage, { session });
    await screen.findByText('Ace');
    expect(screen.queryByRole('button', { name: 'Add from the server' })).toBeNull();
    expect(screen.getAllByRole('button', { name: 'Edit' })).toHaveLength(2);
  });
});

describe("a pilot's account on the server", () => {
  const open = async (callsign: string) => {
    await fireEvent.click(
      await screen.findByRole('button', { name: `Account of ${callsign} on the server` })
    );
    return screen.findByRole('dialog');
  };
  const box = () => screen.getByLabelText('Account') as HTMLSelectElement;

  it('is picked from the members of the club, by name', async () => {
    const { setPilotServerAccountImpl } = linked();
    const dialog = await open('Ace');
    expect(dialog).toHaveTextContent('Who is Ace on the server?');
    await waitFor(() =>
      expect([...box().options].map((o) => o.textContent)).toEqual(['Nobody', 'Pia', 'Quin'])
    );
    expect(box().value).toBe('');
    // Nothing to save until something is picked.
    expect(within(dialog).getByRole('button', { name: 'Save' })).toBeDisabled();
    await fireEvent.change(box(), { target: { value: 'acct_pia' } });
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(setPilotServerAccountImpl).toHaveBeenCalledWith(
      'http://d.local',
      'p1',
      'acct_pia',
      'tok'
    );
    // The list shows who Ace is there, by name. No account id is shown.
    const tag = await screen.findByTestId('pilot-server-account');
    expect(tag).toHaveTextContent('Pia on the server');
    expect(document.body.textContent).not.toContain('acct_');
  });

  it('starts from what the server says now, and can be set back to nobody', async () => {
    const { setPilotServerAccountImpl } = linked({
      pilots: [
        { id: 'p1', callsign: 'Ace', account: PIA },
        { id: 'p2', callsign: 'Bee', account: QUIN }
      ]
    });
    const dialog = await open('Ace');
    await waitFor(() => expect(box().value).toBe('acct_pia'));
    // Quin already holds Bee: the director is told before picking her.
    expect([...box().options].map((o) => o.textContent)).toEqual([
      'Nobody',
      'Pia',
      'Quin (also Bee)'
    ]);
    await fireEvent.change(box(), { target: { value: '' } });
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(setPilotServerAccountImpl).toHaveBeenCalledWith(
        'http://d.local',
        'p1',
        undefined,
        'tok'
      )
    );
  });

  it("shows the server's words when it does not agree, and stays open", async () => {
    const { setPilotServerAccountImpl } = linked();
    setPilotServerAccountImpl.mockRejectedValueOnce(
      new Error('That account is not a member of the club on the server.')
    );
    const dialog = await open('Bee');
    await waitFor(() => expect(box().options).toHaveLength(3));
    await fireEvent.change(box(), { target: { value: 'acct_quin' } });
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    expect(
      await within(dialog).findByText('That account is not a member of the club on the server.')
    ).toBeVisible();
    expect(screen.queryByTestId('pilot-server-account')).toBeNull();
  });

  it('says so when the server cannot be asked, and offers nothing to save', async () => {
    const { getServerPeopleImpl } = linked();
    getServerPeopleImpl.mockRejectedValueOnce(new Error('The server could not be reached.'));
    const dialog = await open('Ace');
    expect(await within(dialog).findByText('The server could not be reached.')).toBeVisible();
    expect(within(dialog).queryByRole('button', { name: 'Save' })).toBeNull();
    expect(within(dialog).queryByLabelText('Account')).toBeNull();
  });

  it('says so when the club has no members yet', async () => {
    linked({ members: [] });
    const dialog = await open('Ace');
    expect(
      await within(dialog).findByText(/Your club has no members on the server yet\./)
    ).toBeVisible();
    expect(within(dialog).queryByRole('button', { name: 'Save' })).toBeNull();
  });
});

describe('adding pilots from the server', () => {
  const open = async () => {
    await fireEvent.click(await screen.findByRole('button', { name: 'Add from the server' }));
    return screen.findByRole('dialog');
  };
  const THEIRS = {
    members: [PIA, QUIN],
    pilots: [
      { id: 'p1', callsign: 'Ace' },
      { id: 'cat-1', callsign: 'Cat', account: PIA },
      { id: 'dog-2', callsign: 'Dog' }
    ]
  };

  it('offers the pilots that are not here and the members with no pilot', async () => {
    linked(THEIRS);
    const dialog = await open();
    const pilots = await within(dialog).findByRole('group', {
      name: 'Pilots your club has on the server'
    });
    // Ace is here already. Cat is Pia's, so Pia needs no new pilot.
    expect(within(pilots).getAllByRole('checkbox')).toHaveLength(2);
    expect(pilots).toHaveTextContent('Cat');
    expect(pilots).toHaveTextContent("Pia's account");
    expect(pilots).not.toHaveTextContent('Ace');
    const members = within(dialog).getByRole('group', { name: 'Members with no pilot yet' });
    expect(within(members).getAllByRole('checkbox')).toHaveLength(1);
    expect(members).toHaveTextContent('Quin');
    // Nothing is picked to begin with, so nothing can be added by a slip.
    expect(within(dialog).getByRole('button', { name: 'Add pilots' })).toBeDisabled();
    expect(document.body.textContent).not.toContain('acct_');
  });

  it('adds what was picked, and the list shows them tied to their accounts', async () => {
    const { pullFromServerImpl } = linked(THEIRS);
    const dialog = await open();
    await fireEvent.click(await within(dialog).findByRole('checkbox', { name: /Cat/ }));
    await fireEvent.click(within(dialog).getByRole('checkbox', { name: 'Quin' }));
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Add 2 pilots' }));

    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(pullFromServerImpl).toHaveBeenCalledWith(
      'http://d.local',
      { pilots: ['cat-1'], members: ['acct_quin'] },
      'tok'
    );
    await waitFor(() => expect(screen.getAllByTestId('pilot-server-account')).toHaveLength(2));
    expect(screen.getByText('Cat')).toBeInTheDocument();
    expect(screen.getAllByTestId('pilot-server-account').map((tag) => tag.textContent)).toEqual([
      'Pia on the server',
      'Quin on the server'
    ]);
  });

  it('picks everything with one press', async () => {
    const { pullFromServerImpl } = linked(THEIRS);
    const dialog = await open();
    await fireEvent.click(await within(dialog).findByRole('button', { name: 'Pick all' }));
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Add 3 pilots' }));
    await waitFor(() =>
      expect(pullFromServerImpl).toHaveBeenCalledWith(
        'http://d.local',
        { pilots: ['cat-1', 'dog-2'], members: ['acct_quin'] },
        'tok'
      )
    );
  });

  it('says so when there is nobody to add', async () => {
    linked({
      members: [PIA],
      pilots: [
        { id: 'p1', callsign: 'Ace', account: PIA },
        { id: 'p2', callsign: 'Bee' }
      ]
    });
    const dialog = await open();
    expect(await within(dialog).findByText(/There is nobody to add\./)).toBeVisible();
    expect(within(dialog).queryByRole('button', { name: /^Add/ })).toBeNull();
  });

  it("shows the server's words when it refuses, and adds nothing", async () => {
    const { pullFromServerImpl, server } = linked(THEIRS);
    pullFromServerImpl.mockRejectedValueOnce(
      new Error('The server no longer has one of those pilots. Look again.')
    );
    const dialog = await open();
    await fireEvent.click(await within(dialog).findByRole('checkbox', { name: /Dog/ }));
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Add 1 pilot' }));
    expect(
      await within(dialog).findByText('The server no longer has one of those pilots. Look again.')
    ).toBeVisible();
    expect(server.pilots).toHaveLength(2);
  });

  it('says so when the server cannot be asked', async () => {
    const { getServerPeopleImpl } = linked(THEIRS);
    getServerPeopleImpl.mockRejectedValueOnce(new Error('The server could not be reached.'));
    const dialog = await open();
    expect(await within(dialog).findByText('The server could not be reached.')).toBeVisible();
    expect(within(dialog).queryByRole('button', { name: /^Add/ })).toBeNull();
  });
});
