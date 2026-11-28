/**
 * More than one club on a server, through real browsers (docs/primitives.md §13.2).
 *
 * The Director is the real `gridfpv` binary with its real console, and the server is the
 * real `gridfpv-server` binary with its real page. The owner signs in to the server
 * through a stand-in sign-in service.
 *
 * One story: the owner makes a second club and a link token for it. The director's Grid
 * sends an event into that club and no other. The club is renamed, and its old address
 * still opens. The owner archives it: it leaves the list, its events still open, and Grid
 * is told why it can send no more. Brought back, Grid carries on.
 */
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import type { Browser, Page } from '@playwright/test';
import { openServerPage, openServerPart } from './serverPages.js';

import { startProvider, type FakeProvider } from '../test-harness/provider.js';
import { startServer, type RemoteServer } from '../test-harness/server.js';
import { linkOnPage, openEvent, unlink } from './link.js';
import { gotoPage } from './nav.js';
import { E2E_EVENT_NAME, expect, test } from './observability.js';

const dist = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'apps', 'rd-console', 'dist');

let provider: FakeProvider;
let server: RemoteServer;

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
// The specs here send the Director's own event, so it is the one that is open.
test.beforeEach(async ({ director }) => {
  await openEvent(director.baseUrl, director.event);
});
// The Director is shared by every spec in the worker: leave it pushing to nothing.
test.afterEach(async ({ director }) => {
  await unlink(director.baseUrl);
});

async function fresh(browser: Browser): Promise<Page> {
  return (await browser.newContext()).newPage();
}

/** The names of the events on the server's front page, for whoever this page is. */
async function listed(page: Page): Promise<string[]> {
  await page.goto(`${server.baseUrl}/#/`);
  await page.reload();
  await expect(
    page.getByTestId('server-event-link').or(page.getByText('No events yet')).first()
  ).toBeVisible();
  return page.getByTestId('server-event-link').locator('.name').allInnerTexts();
}

test('an owner makes a second club, a director sends to it, and it is archived and brought back', async ({
  page,
  browser,
  director
}) => {
  test.setTimeout(240_000);

  // ── The owner signs in and makes a club. ──────────────────────────────────
  const setup = /http:\/\/\S+\/#\/invite\/\S+/.exec(server.run(['owner', 'invite']))![0];
  const ryan = await fresh(browser);
  await ryan.goto(setup);
  // People are told about owners where they sign in.
  await expect(ryan.getByTestId('owners-see-all')).toContainText(
    'The owners of this server can see and change everything on it.'
  );
  provider.person = { sub: 'sub-ryan', name: 'Ryan' };
  await ryan.getByRole('link', { name: 'Continue with Test login' }).click();
  await expect(ryan.getByTestId('server-who')).toHaveText('Ryan');
  await openServerPart(ryan, 'Server');
  const clubs = ryan.getByTestId('server-clubs');
  await expect(clubs.getByTestId('owned-club')).toHaveCount(1);
  await expect(clubs.getByTestId('owned-club')).toContainText('Recon FPV');
  await clubs.getByLabel('New club').fill('North Whoops');
  await clubs.getByRole('button', { name: 'Make the club' }).click();
  await expect(clubs.getByTestId('owned-club')).toHaveCount(2);
  const north = clubs.getByTestId('owned-club').filter({ hasText: 'North Whoops' });
  await expect(north.getByTestId('club-address')).toHaveText(
    `${server.baseUrl}/#/club/north-whoops`
  );

  // ── A link token for the new club, made on its page. ──────────────────────
  await north.getByRole('link', { name: 'North Whoops' }).click();
  // The club is in the address by its word, and the box at the top says which club it is.
  await expect(ryan).toHaveURL(/#\/admin\/club\/north-whoops$/);
  await expect(ryan.getByTestId('club-pick').locator('option:checked')).toHaveText('North Whoops');
  await openServerPage(ryan, 'Settings');
  await expect(ryan.getByTestId('club-own-address')).toHaveText(
    `${server.baseUrl}/#/club/north-whoops`
  );
  await openServerPage(ryan, 'Link tokens');
  await ryan.getByLabel('What it is for').fill('North laptop');
  await ryan.getByRole('button', { name: 'Make a link token' }).click();
  const token = (await ryan.getByTestId('token-secret').innerText()).trim();
  await ryan.getByTestId('token-made').getByRole('button', { name: 'Done' }).click();

  // ── The director's Grid sends into that club, and into no other. ──────────
  await gotoPage(page, 'Server');
  await linkOnPage(page, server.baseUrl, token, E2E_EVENT_NAME);
  await expect(page.getByTestId('link-state')).toHaveText('Connected to North Whoops', {
    timeout: 20_000
  });
  const row = page.getByTestId('link-event').filter({ hasText: E2E_EVENT_NAME });
  await expect(row).toContainText('On the server', { timeout: 20_000 });

  const visitor = await fresh(browser);
  expect(await listed(visitor)).toEqual([E2E_EVENT_NAME]);
  await expect(visitor.getByRole('region', { name: 'North Whoops' })).toContainText(E2E_EVENT_NAME);
  await expect(visitor.getByRole('region', { name: 'Recon FPV' })).toHaveCount(0);
  // The first club's token cannot read it: one club's Director sees nothing of another's.
  const other = await fetch(`${server.baseUrl}/push/events/${director.event}`, {
    headers: { authorization: `Bearer ${server.token}` }
  });
  expect(other.status).toBe(404);

  // The club's own address shows it alone.
  await visitor.goto(`${server.baseUrl}/#/club/north-whoops`);
  const one = visitor.getByTestId('server-one-club');
  await expect(visitor.getByRole('heading', { name: 'North Whoops', level: 1 })).toBeVisible();
  await expect(one.getByTestId('server-event-link')).toHaveCount(1);
  await visitor.goto(`${server.baseUrl}/#/club/recon-fpv`);
  await expect(visitor.getByTestId('server-one-club')).toContainText('No events yet.');
  await visitor.goto(`${server.baseUrl}/#/club/nowhere`);
  await expect(visitor.getByText('This club is not here')).toBeVisible();

  // ── Renamed: a new address, and the old one still opens. ──────────────────
  await openServerPage(ryan, 'Settings');
  await ryan.getByLabel('Name', { exact: true }).fill('Northern Whoops');
  await ryan.getByRole('form', { name: 'Club name' }).getByRole('button', { name: 'Save' }).click();
  // The page goes on to the club's new address, on the same page of it.
  await expect(ryan).toHaveURL(/#\/admin\/club\/northern-whoops\/settings$/);
  await expect(ryan.getByTestId('club-pick').locator('option:checked')).toHaveText(
    'Northern Whoops'
  );
  const northPage = `${server.baseUrl}/#/admin/club/northern-whoops`;
  for (const word of ['northern-whoops', 'north-whoops', 'North-Whoops']) {
    await visitor.goto(`${server.baseUrl}/#/club/${word}`);
    await visitor.reload();
    await expect(visitor.getByRole('heading', { name: 'Northern Whoops', level: 1 })).toBeVisible();
    await expect(visitor.getByTestId('server-one-club')).toContainText(E2E_EVENT_NAME);
  }

  // ── Archived: off the list, still opens, and Grid is told why. ────────────
  await openServerPart(ryan, 'Server');
  const northRow = clubs.getByTestId('owned-club').filter({ hasText: 'Northern Whoops' });
  await northRow.getByRole('button', { name: 'Archive Northern Whoops' }).click();
  const sure = ryan.getByRole('dialog');
  await expect(sure).toContainText('Archive Northern Whoops?');
  await sure.getByRole('button', { name: 'Archive' }).click();
  await expect(sure).toHaveCount(0);
  await expect(northRow).toContainText('Archived');

  expect(await listed(visitor)).toEqual([]);
  await visitor.goto(`${server.baseUrl}/#/club/northern-whoops`);
  await visitor.reload();
  await expect(visitor.getByTestId('server-one-club')).toBeVisible();
  await expect(visitor.getByText('Archived', { exact: true })).toBeVisible();
  await visitor.getByTestId('server-event-link').click();
  await expect(visitor.getByTestId('server-event')).toHaveText(E2E_EVENT_NAME);

  await expect(page.getByTestId('link-state')).toHaveText('Not connected', { timeout: 60_000 });
  await expect(page.getByTestId('link-problem')).toContainText('Northern Whoops is archived');
  // Nothing in it can be changed, even by the owner, and the page says why.
  await ryan.goto(northPage);
  await expect(ryan.getByTestId('club-archived')).toBeVisible();
  await openServerPage(ryan, 'Link tokens');
  await ryan.getByLabel('What it is for').fill('Another laptop');
  await ryan.getByRole('button', { name: 'Make a link token' }).click();
  await expect(
    ryan.getByText(/This club is archived, so nothing in it can be changed/)
  ).toBeVisible();
  // The command line says the same.
  expect(server.run(['club', 'list'])).toMatch(/northern-whoops\s+Northern Whoops\s+archived/);

  // ── Brought back: on the list again, and Grid carries on by itself. ───────
  await openServerPart(ryan, 'Server');
  await northRow.getByRole('button', { name: 'Bring back Northern Whoops' }).click();
  await expect(northRow).not.toContainText('Archived');
  expect(await listed(visitor)).toEqual([E2E_EVENT_NAME]);
  await expect(page.getByTestId('link-state')).toHaveText('Connected to Northern Whoops', {
    timeout: 60_000
  });
  await expect(row).toContainText('On the server', { timeout: 20_000 });
  expect(server.run(['club', 'list'])).toMatch(/recon-fpv\s+Recon FPV/);

  // ── A pilot in both clubs: the front page opens on their primary club. ────
  const inviteFrom = async (clubPage: string): Promise<string> => {
    await ryan.goto(`${clubPage}/invites`);
    await ryan.reload();
    await ryan.getByLabel('Who it is for').fill('Mateo');
    await ryan.getByRole('button', { name: 'Make the invite' }).click();
    return (await ryan.getByTestId('invite-link').innerText()).trim();
  };
  const mateo = await fresh(browser);
  await mateo.goto(await inviteFrom(`${server.baseUrl}/#/admin/club/club`));
  provider.person = { sub: 'sub-mateo', name: 'Mateo' };
  await mateo.getByRole('link', { name: 'Continue with Test login' }).click();
  await expect(mateo.getByTestId('server-who')).toHaveText('Mateo');
  // Recon FPV was joined first, so it is primary. It has no events.
  await mateo.goto(`${server.baseUrl}/#/`);
  await expect(mateo.getByTestId('no-events')).toBeVisible();
  await expect(mateo.getByRole('heading', { name: 'Recon FPV', level: 1 })).toBeVisible();
  await expect(mateo.getByTestId('server-event-link')).toHaveCount(0);
  // Every club is one step away.
  await openServerPage(mateo, 'Clubs');
  await expect(mateo.getByTestId('server-event-link')).toContainText(E2E_EVENT_NAME);

  // Mateo joins the other club too, and makes it primary.
  await mateo.goto(await inviteFrom(northPage));
  await mateo.getByRole('button', { name: 'Accept as Mateo' }).click();
  await expect(mateo.getByTestId('my-club')).toHaveCount(2);
  await expect(mateo.getByTestId('my-club').filter({ hasText: 'Recon FPV' })).toContainText(
    'Primary'
  );
  await mateo.getByRole('button', { name: 'Make Northern Whoops your primary club' }).click();
  await expect(mateo.getByTestId('my-club').filter({ hasText: 'Northern Whoops' })).toContainText(
    'Primary'
  );
  await mateo.goto(`${server.baseUrl}/#/`);
  await mateo.reload();
  await expect(mateo.getByTestId('server-event-link')).toContainText(E2E_EVENT_NAME);
  await expect(mateo.getByRole('heading', { name: 'Northern Whoops', level: 1 })).toBeVisible();
});
