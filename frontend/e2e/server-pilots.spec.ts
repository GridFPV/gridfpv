/**
 * Pilots, accounts and results between a Director and a remote server, through real
 * browsers (docs/primitives.md §13.7).
 *
 * The Director is the real `gridfpv` binary with its real console, and the server is the
 * real `gridfpv-server` binary with its real page. People sign in to the server through a
 * stand-in sign-in service. Each person is a browser of their own.
 *
 * One story: a director races an event, ties a pilot to a member's account from the
 * Pilots page, and that member sees where they placed. A claim made on the server shows
 * up on the Director. Pilots the server has, and the Director does not, are brought in.
 */
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import type { Browser, Page } from '@playwright/test';
import { openServerPage, openServerPart } from './serverPages.js';

import { startProvider, type FakeProvider, type Person } from '../test-harness/provider.js';
import { startServer, type RemoteServer } from '../test-harness/server.js';
import { linkOnPage, unlink } from './link.js';
import { gotoPage } from './nav.js';
import { expect, test } from './observability.js';

const dist = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'apps', 'rd-console', 'dist');

let provider: FakeProvider;
let server: RemoteServer;
/** What this spec added to the shared Director, to take away again. */
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
  const left = (await (await send('GET', '/pilots')).json()) as { id: string; callsign: string }[];
  for (const pilot of left) {
    // Those made by hand, and those the page brought in.
    if (made.pilots.includes(pilot.id) || ['Zed', 'Ray'].includes(pilot.callsign)) {
      await send('DELETE', `/pilots/${pilot.id}`);
    }
  }
  made.pilots.length = 0;
});

async function fresh(browser: Browser): Promise<Page> {
  return (await browser.newContext()).newPage();
}

async function continueAs(page: Page, who: Person): Promise<void> {
  provider.person = who;
  await page.getByRole('link', { name: 'Continue with Test login' }).click();
}

/** On a club's page on the server: make a plain invite, and give back its link. */
async function invite(page: Page, label: string): Promise<string> {
  await openServerPage(page, 'Invites');
  await page.getByLabel('Who it is for').fill(label);
  await page.getByRole('button', { name: 'Make the invite' }).click();
  const link = (await page.getByTestId('invite-link').innerText()).trim();
  await page.getByRole('button', { name: 'Done' }).click();
  return link;
}

/** Someone joins the club on the server with an invite the owner makes. */
async function join(browser: Browser, owner: Page, who: Person): Promise<Page> {
  const page = await fresh(browser);
  await page.goto(await invite(owner, who.name));
  await continueAs(page, who);
  await expect(page.getByTestId('server-who')).toHaveText(who.name);
  return page;
}

/**
 * On a club's page on the server: who holds the pilot with this callsign. The row is found
 * by the pilot's own name, since every row also lists the members it could be given to.
 */
function holderOf(page: Page, callsign: string) {
  return page
    .getByTestId('club-pilot')
    .filter({ has: page.locator('.who', { hasText: new RegExp(`^${callsign}$`) }) })
    .getByTestId('pilot-holder');
}

test('a pilot tied to an account on the Pilots page sees where they placed', async ({
  page,
  browser,
  director
}) => {
  test.setTimeout(120_000);
  const post = async (path: string, body: unknown, method = 'POST'): Promise<{ id: string }> => {
    const answer = await fetch(`${director.baseUrl}${path}`, {
      method,
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body)
    });
    const text = await answer.text();
    expect(answer.ok, `${method} ${path}: ${answer.status} ${text}`).toBe(true);
    return (text ? JSON.parse(text) : {}) as { id: string };
  };

  // ── The director's race day: two pilots, one heat, raced and saved. ───────
  const swift = (await post('/pilots', { callsign: 'Swift' })).id;
  const tardy = (await post('/pilots', { callsign: 'Tardy' })).id;
  made.pilots.push(swift, tardy);
  const open = (await post('/classes', { name: 'Open', source: 'Custom' })).id;
  const event = (await post('/events', { name: 'Claims Cup' })).id;
  made.events.push(event);
  await post('/active-event', { id: event }, 'PUT');
  const commands = `/events/${event}/commands`;
  await post(commands, { Record: { RosterSet: { class: open, pilots: [swift, tardy] } } });
  await post(commands, {
    Record: {
      PhaseDefined: {
        def: {
          id: 'heat-1',
          label: 'Heat 1',
          // The heat is the Open class's, so it counts toward that class's standings.
          class: open,
          kind: {
            Heat: {
              lineup: [
                { adapter: 'bench', seat: 'seat-1', source: { Pilot: swift } },
                { adapter: 'bench', seat: 'seat-2', source: { Pilot: tardy } }
              ]
            }
          },
          format: {
            name: 'Open',
            race: { end: 'Open', staging_micros: 0, first_lap: 'HoleShot' },
            // Most laps wins, so the heat places its pilots and counts for the class.
            ranking: { sort: ['Laps'] },
            completion: 'DirectorDecides'
          }
        }
      }
    }
  });
  await post(commands, { StartRace: { heat: 'heat-1' } });
  await new Promise((done) => setTimeout(done, 100));
  // Swift crosses four times and Tardy twice: Swift wins on laps.
  let at = Date.now() * 1000;
  for (const seat of ['seat-1', 'seat-2', 'seat-1', 'seat-2', 'seat-1', 'seat-1']) {
    await post(`/events/${event}/passes`, { adapter: 'bench', seat, at });
    at += 1_500_000;
  }
  await post(commands, { StopRace: { race: 'heat-1#1' } });
  await post(commands, { SaveRace: { race: 'heat-1#1' } });

  // ── The server: an owner, and two members with nothing to their names. ────
  const setup = /http:\/\/\S+\/#\/invite\/\S+/.exec(server.run(['owner', 'invite']))![0];
  const ryan = await fresh(browser);
  await ryan.goto(setup);
  await continueAs(ryan, { sub: 'sub-ryan', name: 'Ryan' });
  await ryan.getByTestId('server-who').click();
  await openServerPart(ryan, 'Club');
  const pia = await join(browser, ryan, { sub: 'sub-pia', name: 'Pia' });
  const quin = await join(browser, ryan, { sub: 'sub-quin', name: 'Quin' });
  await join(browser, ryan, { sub: 'sub-ray', name: 'Ray' });

  // ── The director links the server, and the event goes up. ─────────────────
  await gotoPage(page, 'Server');
  await linkOnPage(page, server.baseUrl, server.token, 'Claims Cup');
  await expect(page.getByTestId('link-event').filter({ hasText: 'Claims Cup' })).toContainText(
    'On the server',
    { timeout: 20_000 }
  );

  // ── On the Pilots page, Swift is tied to Pia: picked by name. ─────────────
  await gotoPage(page, 'Pilots');
  const row = (callsign: string) =>
    page.getByRole('listitem').filter({ has: page.getByText(callsign, { exact: true }) });
  await page.getByRole('button', { name: 'Account of Swift on the server' }).click();
  const picker = page.getByRole('dialog');
  await expect(picker).toContainText('Who is Swift on the server?');
  await picker.getByLabel('Account').selectOption({ label: 'Pia' });
  await picker.getByRole('button', { name: 'Save' }).click();
  await expect(picker).toHaveCount(0);
  await expect(row('Swift').getByTestId('pilot-server-account')).toHaveText('Pia on the server');
  await expect(row('Tardy').getByTestId('pilot-server-account')).toHaveCount(0);

  // ── Pia sees the event she flew, and that she won it. ─────────────────────
  await pia.goto(`${server.baseUrl}/#/results`);
  const result = pia.getByTestId('result').filter({ hasText: 'Claims Cup' });
  await expect(result).toContainText('Flew as Swift');
  await expect(result.getByTestId('result-place')).toHaveText('Open: 1st of 2');
  // Quin holds nobody, so has no results.
  await quin.goto(`${server.baseUrl}/#/results`);
  await expect(quin.getByText('No results yet.')).toBeVisible();

  // ── A claim made on the server shows up on the Director by itself. ────────
  await ryan.reload();
  await openServerPage(ryan, 'Pilots');
  await ryan.getByLabel('Give Tardy to').selectOption({ label: 'Quin' });
  await expect(holderOf(ryan, 'Tardy')).toHaveText('Quin');
  await expect(async () => {
    await page.reload();
    await expect(row('Tardy').getByTestId('pilot-server-account')).toHaveText(
      'Quin on the server',
      { timeout: 2_000 }
    );
  }).toPass({ timeout: 40_000 });
  await quin.reload();
  await expect(quin.getByTestId('result-place')).toHaveText('Open: 2nd of 2');

  // ── Bringing pilots in. Another Director of the club sent Zed; Ray has no pilot. ──
  const sent = await fetch(`${server.baseUrl}/push/pilots`, {
    method: 'PUT',
    headers: { authorization: `Bearer ${server.token}`, 'content-type': 'application/json' },
    body: JSON.stringify([{ id: 'zed-9', callsign: 'Zed' }])
  });
  expect(sent.ok).toBe(true);
  await page.getByRole('button', { name: 'Add from the server' }).click();
  const bring = page.getByRole('dialog');
  // Swift and Tardy are here, and Pia and Quin have pilots: only Zed and Ray are offered.
  await expect(bring.getByRole('checkbox')).toHaveCount(2);
  await expect(bring.getByRole('checkbox', { name: 'Zed' })).toBeVisible();
  await expect(bring.getByRole('checkbox', { name: 'Ray' })).toBeVisible();
  await bring.getByRole('button', { name: 'Pick all' }).click();
  await bring.getByRole('button', { name: 'Add 2 pilots' }).click();
  await expect(bring).toHaveCount(0);
  await expect(row('Zed')).toBeVisible();
  await expect(row('Ray').getByTestId('pilot-server-account')).toHaveText('Ray on the server');
  // Zed kept the id it has on the server, and the server ties Ray's new pilot to Ray.
  const here = (await (await fetch(`${director.baseUrl}/pilots`)).json()) as { id: string }[];
  expect(here.map((pilot) => pilot.id)).toContain('zed-9');
  await ryan.reload();
  await expect(holderOf(ryan, 'Ray')).toHaveText('Ray');

  // ── The account wins. Pia calls herself PiaFPV on the server. ─────────────
  // The event was run as Swift. Her results show under her own callsign at once.
  await pia.getByTestId('server-who').click();
  await pia.getByTestId('pilot-profile').getByRole('button', { name: 'Fill it in' }).click();
  await pia.getByLabel('Callsign').fill('PiaFPV');
  await pia.getByLabel('MultiGP id').fill('24680');
  await pia.getByTestId('pilot-profile').getByRole('button', { name: 'Save' }).click();
  await expect(pia.getByTestId('profile-callsign')).toHaveText('PiaFPV');
  await pia.goto(`${server.baseUrl}/#/results`);
  await expect(pia.getByTestId('result').filter({ hasText: 'Claims Cup' })).toContainText(
    'Flew as PiaFPV'
  );
  // A visitor looking at the event sees her callsign too, and never her MultiGP id.
  const shown = await fetch(`${server.baseUrl}/api/clubs/club/events/${event}/public/event/pilots`);
  const shownText = await shown.text();
  expect(shownText).toContain('PiaFPV');
  expect(shownText).not.toContain('Swift');
  expect(shownText).not.toContain('24680');

  // Grid takes it at its next pull: the pilot is PiaFPV here too, with her MultiGP id.
  await expect(async () => {
    await page.reload();
    await expect(row('PiaFPV').getByTestId('pilot-server-account')).toHaveText(
      'Pia on the server',
      { timeout: 2_000 }
    );
  }).toPass({ timeout: 40_000 });
  await expect(row('Swift')).toHaveCount(0);
  const mine = (await (await fetch(`${director.baseUrl}/pilots`)).json()) as {
    callsign: string;
    multigp_id?: string;
  }[];
  expect(mine.find((pilot) => pilot.callsign === 'PiaFPV')?.multigp_id).toBe('24680');
  // The editor says where the callsign comes from.
  await row('PiaFPV').getByRole('button', { name: /Edit/ }).click();
  await expect(page.getByTestId('pilot-from-account')).toContainText(
    'This pilot is Pia on the server.'
  );
  await page.getByRole('dialog').getByRole('button', { name: 'Cancel' }).click();
  await expect(page.getByTestId('pilot-from-account')).toBeHidden();

  // ── Untying. Pia's results are no longer hers. The pilot keeps the callsign. ──
  await page.getByRole('button', { name: 'Account of PiaFPV on the server' }).click();
  await expect(page.getByRole('dialog').getByLabel('Account')).not.toHaveValue('');
  await page.getByRole('dialog').getByLabel('Account').selectOption({ label: 'Nobody' });
  await page.getByRole('dialog').getByRole('button', { name: 'Save' }).click();
  await expect(row('PiaFPV').getByTestId('pilot-server-account')).toHaveCount(0);
  await pia.reload();
  await expect(pia.getByText('No results yet.')).toBeVisible();
});
