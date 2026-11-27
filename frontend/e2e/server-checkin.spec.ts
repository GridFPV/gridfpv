/**
 * Checking in to an open seat from a remote server's page, through real browsers
 * (docs/primitives.md §11.9, §13.13).
 *
 * The Director is the real `gridfpv` binary with its real console, and the server is the
 * real `gridfpv-server` binary with its real page. A pilot signs in to the server through
 * a stand-in sign-in service, on a browser of their own.
 *
 * One story: practice is being flown with open seats. The director shows the race code on
 * the Pilot codes page. A pilot on the server's page asks for a seat, is asked for the
 * code, gets it wrong, gets it right, and has the seat. A new code ends the old one. The
 * QR code's link carries the code, so a pilot who scans it types nothing.
 */
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import type { Browser, Page } from '@playwright/test';

import { startProvider, type FakeProvider, type Person } from '../test-harness/provider.js';
import { startServer, type RemoteServer } from '../test-harness/server.js';
import { linkTo, unlink } from './link.js';
import { openPage } from './nav.js';
import { expect, test } from './observability.js';
import { openServerPage, openServerPart } from './serverPages.js';

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

test('a pilot takes a seat from the server with the race code shown at the race', async ({
  page,
  browser,
  director
}) => {
  test.setTimeout(150_000);
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

  // ── The director's event: open practice, being flown. ─────────────────────
  const dash = (await call<WithId>('/pilots', { callsign: 'Dash' })).id;
  const echo = (await call<WithId>('/pilots', { callsign: 'Echo' })).id;
  made.pilots.push(dash, echo);
  const open = (await call<WithId>('/classes', { name: 'Open', source: 'Custom' })).id;
  const event = (await call<WithId>('/events', { name: 'Practice Night' })).id;
  made.events.push(event);
  await call('/active-event', { id: event }, 'PUT');
  const commands = `/events/${event}/commands`;
  await call(commands, { Record: { RosterSet: { class: open, pilots: [dash, echo] } } });
  await call(commands, {
    Record: {
      PhaseDefined: {
        def: {
          id: 'practice',
          label: 'Open Practice',
          kind: {
            Heat: {
              lineup: [
                { adapter: 'bench', seat: 'seat-1', source: 'Open' },
                { adapter: 'bench', seat: 'seat-2', source: 'Open' }
              ]
            }
          },
          format: {
            name: 'Open practice',
            race: { end: 'Open', staging_micros: 0, first_lap: 'HoleShot' },
            completion: 'DirectorDecides'
          }
        }
      }
    }
  });
  await call(commands, { StartRace: { heat: 'practice' } });
  /** Who has each seat, as the Director has it: a pilot's id, or null. */
  const holders = async (base = director.baseUrl, path = `/events/${event}/state`) => {
    const state = (await (await fetch(`${base}${path}`)).json()) as {
      races: { seats: { holder?: string }[] }[];
    };
    return state.races[0].seats.map((seat) => seat.holder ?? null);
  };
  expect(await holders()).toEqual([null, null]);

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
    .toContain('Practice Night');

  // ── The director's race code, on the Pilot codes page. Closed until opened. ──
  await page.goto(director.baseUrl);
  await openPage(page, 'Pilot codes');
  const card = page.getByTestId('race-code');
  await expect(card.getByTestId('race-code-text')).toHaveCount(0);
  await card.getByRole('button', { name: 'Show' }).click();
  const shown = (await card.getByTestId('race-code-text').innerText()).trim();
  expect(shown).toMatch(/^[A-Z2-9]{3} [A-Z2-9]{3}$/);
  const code = shown.replace(' ', '');
  // Linked and connected, so there is a QR code to scan.
  await card.getByRole('button', { name: 'Show the QR code' }).click();
  const qr = page.getByRole('dialog').getByRole('img');
  await expect(qr).toBeVisible();
  await expect
    .poll(() => qr.evaluate((img: HTMLImageElement) => img.naturalWidth))
    .toBeGreaterThan(0);
  await page.getByRole('dialog').getByRole('button', { name: 'Done' }).click();

  // ── Pia asks for a seat on the server's page, and is asked for the code. ──
  await pia.goto(`${server.baseUrl}/#/event/club/${event}`);
  await pia.getByRole('tab', { name: 'Me' }).click();
  const fly = pia.getByRole('region', { name: 'Fly now' });
  await fly.getByRole('button', { name: 'Take Seat 1' }).click();
  const ask = pia.getByRole('dialog');
  await expect(ask).toContainText('Are you at the race?');
  expect(await holders()).toEqual([null, null]);

  // A wrong code: the Director says so, and no seat is taken.
  const wrong = code === 'AAAAAA' ? 'BBBBBB' : 'AAAAAA';
  await ask.getByLabel('Race code').fill(wrong);
  await ask.getByRole('button', { name: 'Take the seat' }).click();
  await expect(ask.getByTestId('race-code-why')).toContainText('That is not the race code');
  expect(await holders()).toEqual([null, null]);

  // The right code, typed the way a person would.
  await ask.getByLabel('Race code').fill(`${code.slice(0, 3).toLowerCase()} ${code.slice(3)}`);
  await ask.getByRole('button', { name: 'Take the seat' }).click();
  await expect(ask).toHaveCount(0, { timeout: 15_000 });
  await expect.poll(() => holders(), { timeout: 15_000 }).toEqual([dash, null]);
  await expect(fly).toContainText('You have Seat 1', { timeout: 15_000 });
  // The server shows it to anyone, and the code is nowhere in what it gives out.
  const publicState = `/api/clubs/club/events/${event}/public/event/state`;
  await expect
    .poll(() => holders(server.baseUrl, publicState), { timeout: 15_000 })
    .toEqual([dash, null]);
  const given = await (await fetch(`${server.baseUrl}${publicState}`)).text();
  expect(given).not.toContain(code);

  // ── The next seat needs no typing: the browser kept the code. ─────────────
  await fly.getByRole('button', { name: 'Take Seat 2' }).click();
  await expect.poll(() => holders(), { timeout: 15_000 }).toEqual([null, dash]);
  await expect(pia.getByRole('dialog')).toHaveCount(0);
  await expect(fly).toContainText('You have Seat 2', { timeout: 15_000 });

  // ── Her laps on the seat are hers. ────────────────────────────────────────
  let at = Date.now() * 1000;
  for (let i = 0; i < 3; i++) {
    await call(`/events/${event}/passes`, { adapter: 'bench', seat: 'seat-2', at });
    at += 2_000_000;
  }
  const state = await call<{
    races: { seats: { stints: { pilot: string; laps: unknown[] }[] }[] }[];
  }>(`/events/${event}/state`, undefined, 'GET');
  const stints = state.races[0].seats[1].stints;
  expect(stints.at(-1)!.pilot).toBe(dash);
  expect(stints.at(-1)!.laps).toHaveLength(2);

  // ── She drops the seat. No code is asked for. ─────────────────────────────
  await fly.getByRole('button', { name: 'Drop Seat 2' }).click();
  await expect.poll(() => holders(), { timeout: 15_000 }).toEqual([null, null]);
  await expect(pia.getByRole('dialog')).toHaveCount(0);

  // ── The director makes a new code. The one her browser kept stops working. ──
  await card.getByRole('button', { name: 'New code…' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'New code' }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(card.getByTestId('race-code-text')).not.toHaveText(shown);
  const newCode = (await card.getByTestId('race-code-text').innerText()).replace(' ', '').trim();
  await expect(fly.getByRole('button', { name: 'Take Seat 1' })).toBeVisible({ timeout: 15_000 });
  await fly.getByRole('button', { name: 'Take Seat 1' }).click();
  await expect(pia.getByRole('dialog').getByTestId('race-code-why')).toContainText(
    'That is not the race code'
  );
  expect(await holders()).toEqual([null, null]);
  await pia.getByRole('dialog').getByRole('button', { name: 'Cancel' }).click();
  await expect(pia.getByRole('dialog')).toHaveCount(0);

  // ── She scans the QR code instead: its link carries the new code. ─────────
  const view = await call<{ code: string; link?: string }>(
    `/events/${event}/race-code`,
    undefined,
    'GET'
  );
  expect(view.code).toBe(newCode);
  expect(view.link).toBe(`${server.baseUrl}/#/here/club/${event}/${newCode}`);
  await pia.goto(view.link!);
  await expect(pia.getByTestId('server-event')).toHaveText('Practice Night');
  // The code is taken out of the address.
  await expect.poll(() => new URL(pia.url()).hash).toBe(`#/event/club/${event}`);
  await pia.getByRole('tab', { name: 'Me' }).click();
  await pia
    .getByRole('region', { name: 'Fly now' })
    .getByRole('button', { name: 'Take Seat 1' })
    .click();
  await expect.poll(() => holders(), { timeout: 15_000 }).toEqual([dash, null]);
  await expect(pia.getByRole('dialog')).toHaveCount(0);

  // ── The server turns away anyone it does not take for a pilot in the event. ──
  const body = { race: 'practice#1', adapter: 'bench', seat: 'seat-2', code: newCode };
  const take = `${server.baseUrl}/api/clubs/club/events/${event}/public/pilot/checkin/take`;
  const onlooker = await fresh(browser);
  expect((await onlooker.request.post(take, { data: body })).status()).toBe(401);
  // Ryan is signed in and knows the code, but holds no pilot in the event.
  expect((await ryan.request.post(take, { data: body })).status()).toBe(403);
  expect(await holders()).toEqual([dash, null]);
});
