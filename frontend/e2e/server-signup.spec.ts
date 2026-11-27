/**
 * A pilot signing up for a heat on the remote server, through real browsers
 * (docs/primitives.md §13.4, §11.5).
 *
 * The Director is the real `gridfpv` binary and the server is the real `gridfpv-server`
 * binary. The pilot is in a browser on the server's own page, far from the race. What
 * they ask for is decided by the Director, the only writer of the event.
 */
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import type { Browser, Page } from '@playwright/test';
import { openServerPage, openServerPart } from './serverPages.js';

import { startProvider, type FakeProvider, type Person } from '../test-harness/provider.js';
import { startServer, type RemoteServer } from '../test-harness/server.js';
import { linkTo, sendEventTo, unlink } from './link.js';
import { expect, test } from './observability.js';

const dist = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'apps', 'rd-console', 'dist');

let provider: FakeProvider;
let server: RemoteServer;
const made = { pilots: [] as string[], events: [] as string[] };

test.beforeAll(async () => {
  provider = await startProvider();
  server = await startServer('Recon FPV', dist, (baseUrl) => ({
    GRIDFPV_SERVER_PUBLIC_URL: baseUrl,
    ...provider.settings()
  }));
});
test.afterAll(async () => {
  await server?.stop();
  await provider?.stop();
});
// The Director is shared by every spec in the worker: leave it as it was found.
test.afterEach(async ({ director }) => {
  const send = (method: string, path: string, body?: unknown) =>
    fetch(`${director.baseUrl}${path}`, {
      method,
      headers: { 'Content-Type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body)
    });
  await unlink(director.baseUrl);
  await send('PUT', '/active-event', { id: director.event });
  for (const id of made.events.splice(0)) await send('DELETE', `/events/${id}`);
  for (const id of made.pilots.splice(0)) await send('DELETE', `/pilots/${id}`);
});

async function fresh(browser: Browser): Promise<Page> {
  return (await browser.newContext()).newPage();
}

async function continueAs(page: Page, who: Person): Promise<void> {
  provider.person = who;
  await page.getByRole('link', { name: 'Continue with Test login' }).click();
}

test('a pilot signs up on the server, and the Director decides', async ({ browser, director }) => {
  test.setTimeout(120_000);
  const call = async <T>(path: string, body?: unknown, method = 'POST'): Promise<T> => {
    const answer = await fetch(`${director.baseUrl}${path}`, {
      method,
      headers: { 'Content-Type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body)
    });
    const text = await answer.text();
    expect(answer.ok, `${method} ${path}: ${answer.status} ${text}`).toBe(true);
    return (text ? JSON.parse(text) : {}) as T;
  };
  type WithId = { id: string };

  // ── The director's event: qualifying, with sign-up open. ──────────────────
  const dash = (await call<WithId>('/pilots', { callsign: 'Dash' })).id;
  const echo = (await call<WithId>('/pilots', { callsign: 'Echo' })).id;
  made.pilots.push(dash, echo);
  const open = (await call<WithId>('/classes', { name: 'Open', source: 'Custom' })).id;
  const event = (await call<WithId>('/events', { name: 'Sign-up Night' })).id;
  made.events.push(event);
  await call('/active-event', { id: event }, 'PUT');
  const commands = `/events/${event}/commands`;
  await call(commands, { Record: { RosterSet: { class: open, pilots: [dash, echo] } } });
  await call(commands, {
    Record: {
      PhaseDefined: {
        def: {
          id: 'quali',
          label: 'Qualifying',
          kind: { Group: { field: [{ Class: open }], combine: 'Stack' } },
          format: {
            name: 'One race each',
            race: { end: 'Open', staging_micros: 0, first_lap: 'HoleShot' },
            completion: { SavedRaces: 1 },
            ranking: { sort: ['LastFinish'] }
          }
        }
      }
    }
  });
  const seat = (n: number) => ({ adapter: 'bench', seat: `seat-${n}` });
  await call(commands, {
    Signup: {
      Set: { phase: 'quali', rules: { seats: [seat(1), seat(2)], open_heats: 2, open: true } }
    }
  });
  /** Who is in the first heat, as the Director has it: a pilot's id, or null, per seat. */
  const firstHeat = async (): Promise<(string | null)[]> => {
    const state = await call<{
      phases: { def: { id: string }; signup?: { heats: { seats: (string | null)[] }[] } }[];
    }>(`/events/${event}/state`, undefined, 'GET');
    return state.phases.find((phase) => phase.def.id === 'quali')!.signup!.heats[0].seats;
  };
  expect(await firstHeat()).toEqual([null, null]);

  // ── The server: an owner, and Pia, a member. ──────────────────────────────
  const setup = /http:\/\/\S+\/#\/invite\/\S+/.exec(server.run(['owner', 'invite']))![0];
  const ryan = await fresh(browser);
  await ryan.goto(setup);
  await continueAs(ryan, { sub: 'sub-ryan', name: 'Ryan' });
  await ryan.getByTestId('server-who').click();
  await openServerPart(ryan, 'Club');
  await openServerPage(ryan, 'Invites');
  await ryan.getByLabel('Who it is for').fill('Pia');
  await ryan.getByRole('button', { name: 'Make the invite' }).click();
  const link = (await ryan.getByTestId('invite-link').innerText()).trim();
  const pia = await fresh(browser);
  await pia.goto(link);
  await continueAs(pia, { sub: 'sub-pia', name: 'Pia' });
  await expect(pia.getByTestId('server-who')).toHaveText('Pia');

  // ── The director links the server and ties Dash to Pia. ───────────────────
  await linkTo(director.baseUrl, server.baseUrl, server.token, event);
  const people = await call<{ members: { id: string; name: string }[] }>(
    '/server-link/people',
    undefined,
    'GET'
  );
  const piasAccount = people.members.find((member) => member.name === 'Pia')!.id;
  await call(`/pilots/${dash}/server-account`, { account: piasAccount }, 'PUT');
  await expect
    .poll(
      async () => {
        const list = (await (await fetch(`${server.baseUrl}/api/clubs/club/events`)).json()) as {
          name: string;
        }[];
        return list.map((entry) => entry.name);
      },
      { timeout: 20_000 }
    )
    .toContain('Sign-up Night');

  // ── Someone who is only looking sees the event, and no way to sign up. ────
  const onlooker = await fresh(browser);
  await onlooker.goto(`${server.baseUrl}/#/event/club/${event}`);
  await expect(onlooker.getByTestId('server-event')).toHaveText('Sign-up Night');
  await expect(onlooker.getByRole('tab', { name: 'Results' })).toBeVisible();
  await expect(onlooker.getByRole('tab', { name: 'Sign up' })).toHaveCount(0);

  // ── Pia, signed in and tied to Dash, signs up from the server's page. ─────
  await pia.goto(`${server.baseUrl}/#/event/club/${event}`);
  await pia.getByRole('tab', { name: 'Sign up' }).click();
  const take = pia.getByRole('button', { name: /^Take .+ in Qualifying Heat 1$/ });
  await expect(take).toHaveCount(2);
  await take.nth(1).click();
  // The Director decided it: Dash is on the second seat of the first heat.
  await expect.poll(firstHeat, { timeout: 15_000 }).toEqual([null, dash]);
  // And the server's page shows it to her, with no refresh.
  await expect(pia.getByRole('button', { name: 'Leave Qualifying Heat 1' })).toBeVisible({
    timeout: 15_000
  });
  await expect(pia.getByRole('button', { name: /^Take .+ in Qualifying Heat 1$/ })).toHaveCount(0);
  // What anyone reads from the server says the same as the Director.
  const onServer = async (): Promise<(string | null)[]> => {
    const state = (await (
      await fetch(`${server.baseUrl}/api/clubs/club/events/${event}/public/event/state`)
    ).json()) as {
      phases: { def: { id: string }; signup?: { heats: { seats: (string | null)[] }[] } }[];
    };
    return state.phases.find((phase) => phase.def.id === 'quali')!.signup!.heats[0].seats;
  };
  await expect.poll(onServer, { timeout: 15_000 }).toEqual([null, dash]);

  // The server itself refuses anyone it does not take for a pilot in the event.
  const stranger = await onlooker.request.post(
    `${server.baseUrl}/api/clubs/club/events/${event}/public/pilot/signup/take`,
    { data: { heat: 'quali/heat-1', seat: seat(1) } }
  );
  expect(stranger.status()).toBe(401);
  const notAPilot = await ryan.request.post(
    `${server.baseUrl}/api/clubs/club/events/${event}/public/pilot/signup/take`,
    { data: { heat: 'quali/heat-1', seat: seat(1) } }
  );
  expect(notAPilot.status()).toBe(403);
  expect(await firstHeat()).toEqual([null, dash]);

  // ── She gives the spot up again. ──────────────────────────────────────────
  await pia.getByRole('button', { name: 'Leave Qualifying Heat 1' }).click();
  await expect.poll(firstHeat, { timeout: 15_000 }).toEqual([null, null]);
  await expect(pia.getByRole('button', { name: /^Take .+ in Qualifying Heat 1$/ })).toHaveCount(2, {
    timeout: 15_000
  });

  // ── The director closes sign-up. Her ask is refused, in the Director's words. ──
  await call(commands, {
    Signup: {
      Set: { phase: 'quali', rules: { seats: [seat(1), seat(2)], open_heats: 2, open: false } }
    }
  });
  const refused = await pia.request.post(
    `${server.baseUrl}/api/clubs/club/events/${event}/public/pilot/signup/take`,
    { data: { heat: 'quali/heat-1', seat: seat(1) } }
  );
  expect(refused.status()).toBe(409);
  expect(((await refused.json()) as { message: string }).message.length).toBeGreaterThan(5);
  expect(await firstHeat()).toEqual([null, null]);

  // ── The race computer goes away. She is told so, at once. ─────────────────
  await sendEventTo(director.baseUrl, event, null);
  await expect
    .poll(
      async () => {
        const answer = await pia.request.post(
          `${server.baseUrl}/api/clubs/club/events/${event}/public/pilot/signup/take`,
          { data: { heat: 'quali/heat-1', seat: seat(1) } }
        );
        return ((await answer.json()) as { message?: string }).message ?? '';
      },
      { timeout: 30_000 }
    )
    .toContain('not connected');
});
