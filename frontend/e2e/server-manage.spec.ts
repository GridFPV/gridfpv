/**
 * Running a club from the server's page, through real browsers (docs/primitives.md §13.2,
 * §13.6, §13.9).
 *
 * The Director is the real `gridfpv` binary with its real console, and the server is the
 * real `gridfpv-server` binary with its real page. The owner signs in to the server
 * through a stand-in sign-in service.
 *
 * One story: the owner makes a link token on the page, the director's Grid sends an event
 * with it, the owner hides the event, shows it, and removes it. Grid says the event was
 * removed and does not put it back until the director says to. Then the token is taken
 * back, and Grid can send no more.
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

/** The names of the events the server lists, for whoever this page is. */
async function listed(page: Page): Promise<string[]> {
  await page.goto(`${server.baseUrl}/#/`);
  await page.reload();
  // The list is read after the page is up: wait until it shows events, or says it has none.
  await expect(
    page.getByTestId('server-event-link').or(page.getByText('No events yet')).first()
  ).toBeVisible();
  return page.getByTestId('server-event-link').locator('.name').allInnerTexts();
}

/** What the server itself holds of an event: the status a Director is given. */
async function held(token: string, event: string): Promise<number> {
  const answer = await fetch(`${server.baseUrl}/push/events/${event}`, {
    headers: { authorization: `Bearer ${token}` }
  });
  return answer.status;
}

test('an owner runs the club from the page, and Grid goes along with it', async ({
  page,
  browser,
  director
}) => {
  test.setTimeout(180_000);

  // ── The owner signs in and opens the club. ────────────────────────────────
  const setup = /http:\/\/\S+\/#\/invite\/\S+/.exec(server.run(['owner', 'invite']))![0];
  const ryan = await fresh(browser);
  await ryan.goto(setup);
  provider.person = { sub: 'sub-ryan', name: 'Ryan' };
  await ryan.getByRole('link', { name: 'Continue with Test login' }).click();
  await expect(ryan.getByTestId('server-who')).toHaveText('Ryan');
  await ryan.getByTestId('server-who').click();
  await openServerPart(ryan, 'Club');
  await openServerPage(ryan, 'Events');
  await expect(ryan.getByText('No events yet.')).toBeVisible();
  await openServerPage(ryan, 'Link tokens');
  // The token made on the command line when the server was set up.
  await expect(ryan.getByTestId('link-token')).toHaveCount(1);

  // ── A link token made on the page. It is shown once. ──────────────────────
  await ryan.getByLabel('What it is for').fill("Ryan's laptop");
  await ryan.getByRole('button', { name: 'Make a link token' }).click();
  const token = (await ryan.getByTestId('token-secret').innerText()).trim();
  expect(token).toMatch(/^gfs_[A-Za-z0-9_-]{20,}$/);
  await ryan.getByTestId('token-made').getByRole('button', { name: 'Done' }).click();
  await expect(ryan.getByTestId('link-token')).toHaveCount(2);
  await expect(ryan.getByTestId('link-token').first()).toContainText("Ryan's laptop");
  expect(await ryan.content()).not.toContain(token);
  await ryan.reload();
  await expect(ryan.getByTestId('link-token')).toHaveCount(2);
  expect(await ryan.content()).not.toContain(token);

  // ── The director's Grid sends with it. ────────────────────────────────────
  await gotoPage(page, 'Server');
  await linkOnPage(page, server.baseUrl, token, E2E_EVENT_NAME);
  await expect(page.getByTestId('link-state')).toHaveText('Connected to Recon FPV', {
    timeout: 20_000
  });
  const row = page.getByTestId('link-event').filter({ hasText: E2E_EVENT_NAME });
  await expect(row).toContainText('On the server', { timeout: 20_000 });

  // Anyone sees it listed.
  const visitor = await fresh(browser);
  expect(await listed(visitor)).toEqual([E2E_EVENT_NAME]);

  // ── Hidden: off the list for a visitor, still there for the owner and by its link. ──
  await ryan.reload();
  await openServerPage(ryan, 'Events');
  const event = ryan.getByTestId('manage-event').filter({ hasText: E2E_EVENT_NAME });
  await event.getByRole('button', { name: `Hide ${E2E_EVENT_NAME}` }).click();
  await expect(event).toContainText('Hidden');
  expect(await listed(visitor)).toEqual([]);
  await visitor.goto(`${server.baseUrl}/#/event/club/${director.event}`);
  await expect(visitor.getByTestId('server-event')).toHaveText(E2E_EVENT_NAME);
  // The owner's own list still has it, marked.
  const owner = await ryan.context().newPage();
  expect(await listed(owner)).toEqual([E2E_EVENT_NAME]);
  await expect(owner.getByTestId('server-event-link')).toContainText('Hidden');
  await owner.close();

  // Shown again, it is back for everyone.
  await event.getByRole('button', { name: `Show ${E2E_EVENT_NAME}` }).click();
  await expect(event).not.toContainText('Hidden');
  expect(await listed(visitor)).toEqual([E2E_EVENT_NAME]);

  // ── Removed: gone from the server, and Grid does not put it back. ─────────
  await event.getByRole('button', { name: `Remove ${E2E_EVENT_NAME}` }).click();
  const sure = ryan.getByRole('dialog');
  await expect(sure).toContainText(`Remove ${E2E_EVENT_NAME} from this server?`);
  await sure.getByRole('button', { name: 'Remove' }).click();
  await expect(sure).toHaveCount(0);
  await expect(ryan.getByText('No events yet.')).toBeVisible();
  expect(await listed(visitor)).toEqual([]);
  await visitor.goto(`${server.baseUrl}/#/event/club/${director.event}`);
  await expect(visitor.getByTestId('server-event')).toHaveCount(0);

  // Grid notices by itself, with nothing new to send.
  await expect(row).toContainText('Removed on the server', { timeout: 40_000 });
  // Something new happens at the race. It still does not go up.
  const pilot = (await (
    await fetch(`${director.baseUrl}/pilots`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ callsign: 'Latecomer' })
    })
  ).json()) as { id: string };
  await new Promise((done) => setTimeout(done, 4_000));
  await expect(row).toContainText('Removed on the server');
  expect(await held(token, director.event)).toBe(409);
  expect(await listed(visitor)).toEqual([]);

  // ── The director sends it again. ──────────────────────────────────────────
  await row.getByRole('button', { name: 'Send it again…' }).click();
  const again = page.getByRole('dialog');
  await expect(again).toContainText(`Send ${E2E_EVENT_NAME} to the server again?`);
  await again.getByRole('button', { name: 'Send it again' }).click();
  await expect(again).toHaveCount(0);
  await expect(row).toContainText('On the server', { timeout: 20_000 });
  expect(await held(token, director.event)).toBe(200);
  expect(await listed(visitor)).toEqual([E2E_EVENT_NAME]);
  await fetch(`${director.baseUrl}/pilots/${pilot.id}`, { method: 'DELETE' });

  // ── A new name for the club shows to everyone. ────────────────────────────
  await ryan.reload();
  await openServerPage(ryan, 'Settings');
  await ryan.getByLabel('Name', { exact: true }).fill('Recon Whoop Club');
  await ryan.getByRole('form', { name: 'Club name' }).getByRole('button', { name: 'Save' }).click();
  // The club's address follows its name, and the page goes along.
  await expect(ryan).toHaveURL(/#\/admin\/club\/recon-whoop-club\/settings$/);
  await expect(ryan.getByText('Recon Whoop Club', { exact: true })).toBeVisible();
  await listed(visitor);
  await expect(visitor.getByRole('heading', { name: 'Recon Whoop Club' })).toBeVisible();

  // ── The token is taken back. Grid can send no more, and says why. ─────────
  await openServerPage(ryan, 'Link tokens');
  await ryan.getByRole('button', { name: "Take back the token for Ryan's laptop" }).click();
  await ryan.getByRole('dialog').getByRole('button', { name: 'Take back' }).click();
  await expect(ryan.getByRole('dialog')).toHaveCount(0);
  await expect(ryan.getByTestId('link-token').first()).toContainText('Taken back');
  await expect(page.getByTestId('link-state')).toHaveText('Not connected', { timeout: 40_000 });
  await expect(page.getByTestId('link-problem')).toContainText('did not take the link token');
  expect(await held(token, director.event)).toBe(401);
  // The token from the command line still works: only the one was taken back.
  expect(await held(server.token, director.event)).toBe(200);
});
