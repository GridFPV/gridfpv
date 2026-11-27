/**
 * Sending events to a club on a server, through a real browser (docs/primitives.md §13).
 *
 * The director saves clubs on the Server page of the real console, and says which club
 * each event goes to. The Director is the
 * real `gridfpv` binary and the server is the real `gridfpv-server` binary, so what the
 * page says is on the server is checked by asking the server.
 */
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import type { Page } from '@playwright/test';

import { startServer, type RemoteServer } from '../test-harness/server.js';
import { addClubOnPage, linkOnPage, openEvent, unlink, whereSent } from './link.js';
import { gotoPage } from './nav.js';
import { E2E_EVENT_NAME, expect, test } from './observability.js';

/** The built console. The global setup builds it before any spec runs. */
const dist = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'apps', 'rd-console', 'dist');

let server: RemoteServer;

test.beforeAll(async () => {
  // With the built console, so the server has its own page to show.
  server = await startServer('E2E Club', dist);
});
test.afterAll(async () => {
  await server?.stop();
});
// The specs here send the Director's own event, so it is the one that is open.
test.beforeEach(async ({ director }) => {
  await openEvent(director.baseUrl, director.event);
});
// The Director is shared by every spec in the worker: leave it sending nothing, with its
// own event open and no other.
const made: string[] = [];
test.afterEach(async ({ director }) => {
  await unlink(director.baseUrl);
  await fetch(`${director.baseUrl}/active-event`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ id: director.event })
  });
  for (const id of made.splice(0)) {
    await fetch(`${director.baseUrl}/events/${id}`, { method: 'DELETE' });
  }
});

const state = (page: Page) => page.getByTestId('link-state');
const row = (page: Page, event: string) =>
  page.getByTestId('link-event').filter({ hasText: event });

/** The names of the events a club on the server holds. */
async function eventsOf(club: string): Promise<string[]> {
  const list = (await (await fetch(`${server.baseUrl}/api/clubs/${club}/events`)).json()) as {
    name: string;
  }[];
  return list.map((event) => event.name);
}

test('the Server page is under Setup, and starts with no club saved', async ({ page }) => {
  await gotoPage(page, 'Server');
  await expect.poll(() => new URL(page.url()).hash).toBe('#/server');
  await expect(state(page)).toHaveText('Not sent');
  await expect(page.getByTestId('no-clubs')).toBeVisible();
  await expect(page.getByTestId('link-line')).toContainText('Save a club first');
  // No club to pick: the event's box is closed, and says it is not sent.
  await expect(whereSent(page, E2E_EVENT_NAME)).toBeDisabled();
  await expect(whereSent(page, E2E_EVENT_NAME)).toHaveValue('');
});

test('an event goes to the server once a club is saved and the event is sent to it', async ({
  page,
  director
}) => {
  await gotoPage(page, 'Server');
  await addClubOnPage(page, server.baseUrl, server.token);
  // Saved, and the token is not on the page any more.
  expect(await page.content()).not.toContain(server.token);
  // A saved club sends nothing by itself: the event goes nowhere until the director says.
  await expect(state(page)).toHaveText('Not sent');
  await expect(page.getByTestId('link-line')).toContainText('Pick a club for it under Events');
  await page.waitForTimeout(2_000);
  expect(await eventsOf('club')).toEqual([]);

  await whereSent(page, E2E_EVENT_NAME).selectOption({ index: 1 });
  await expect(state(page)).toHaveText(`Connected to ${server.club}`, { timeout: 15_000 });
  await expect(row(page, E2E_EVENT_NAME)).toContainText('On the server', { timeout: 15_000 });
  // The club is now called what the server calls it, in the list and in the box.
  await expect(page.getByTestId('link-club')).toContainText(server.club);
  await expect(whereSent(page, E2E_EVENT_NAME).locator('option:checked')).toHaveText(server.club);
  await expect(page.getByTestId('link-line')).toHaveText(
    `${E2E_EVENT_NAME} is sent to ${server.club}.`
  );

  // The server holds it, and shows what the Director's own public page shows.
  expect(await eventsOf('club')).toContain(E2E_EVENT_NAME);
  const read = async (base: string): Promise<{ cursor: number; phases: unknown[] }> =>
    (await fetch(`${base}/event/state`)).json() as Promise<{ cursor: number; phases: unknown[] }>;
  await expect
    .poll(async () => {
      const theirs = await read(`${server.baseUrl}/api/clubs/club/events/${director.event}/public`);
      const ours = await read(`${director.baseUrl}/public`);
      return theirs.cursor === ours.cursor && theirs.phases.length === ours.phases.length;
    })
    .toBe(true);

  // It is all there after the page is opened again.
  await page.reload();
  await expect(state(page)).toHaveText(`Connected to ${server.club}`, { timeout: 15_000 });
  expect(await page.content()).not.toContain(server.token);

  // Sent nowhere, it stops, and says so. What is on the server stays.
  await whereSent(page, E2E_EVENT_NAME).selectOption({ index: 0 });
  await expect(state(page)).toHaveText('Not sent');
  await expect(row(page, E2E_EVENT_NAME)).not.toContainText('On the server');
  expect(await eventsOf('club')).toContain(E2E_EVENT_NAME);
});

test("the server's own page lists the event and shows it as a visitor at the race sees it", async ({
  page,
  director
}) => {
  await gotoPage(page, 'Server');
  await linkOnPage(page, server.baseUrl, server.token, E2E_EVENT_NAME);
  await expect(row(page, E2E_EVENT_NAME)).toContainText('On the server', { timeout: 15_000 });

  // Someone opens the server's address. They get its list of events, not a console.
  await page.goto(server.baseUrl);
  const link = page.getByTestId('server-event-link').filter({ hasText: E2E_EVENT_NAME });
  await expect(link).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('heading', { name: server.club })).toBeVisible();
  await expect(page.getByRole('navigation', { name: 'Pages' })).toHaveCount(0);

  await link.click();
  await expect(page.getByTestId('server-event')).toHaveText(E2E_EVENT_NAME);
  await expect.poll(() => new URL(page.url()).hash).toBe(`#/e/club/${director.event}`);
  // The same tabs a visitor at the race gets.
  await expect(page.getByRole('tab').first()).toBeVisible();

  // A refresh stays on the event, and the way back is there.
  await page.reload();
  await expect(page.getByTestId('server-event')).toHaveText(E2E_EVENT_NAME, { timeout: 15_000 });
  await page.getByRole('link', { name: '← All events' }).click();
  await expect(link).toBeVisible();

  // An address for an event that is not there says so.
  await page.goto(`${server.baseUrl}/#/e/club/no-such-event`);
  await expect(page.getByText('This event is not here')).toBeVisible();
});

test('two clubs: Grid talks only to the club of the open event', async ({ page, director }) => {
  test.setTimeout(120_000);
  // A second club on the server, with a link token of its own.
  server.run(['club', 'new', 'North Whoops']);
  const northToken = /gfs_\S+/.exec(server.run(['token', 'new', 'laptop', 'north-whoops']))![0];
  const north = (
    (await (await fetch(`${server.baseUrl}/api/club-words/north-whoops`)).json()) as { id: string }
  ).id;
  // And a second event on the Director.
  const second = (await (
    await fetch(`${director.baseUrl}/events`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: 'North Night' })
    })
  ).json()) as { id: string };
  made.push(second.id);

  await gotoPage(page, 'Server');
  await linkOnPage(page, server.baseUrl, server.token, E2E_EVENT_NAME);
  await expect(state(page)).toHaveText(`Connected to ${server.club}`, { timeout: 15_000 });
  await addClubOnPage(page, server.baseUrl, northToken);
  await expect(page.getByTestId('link-club')).toHaveCount(2);
  await whereSent(page, 'North Night').selectOption({ index: 2 });

  // The open event's club has its event. The other event waits, and North is not told.
  await expect(row(page, E2E_EVENT_NAME)).toContainText('On the server', { timeout: 15_000 });
  await expect(row(page, 'North Night')).toContainText('Waiting');
  await expect(row(page, 'North Night')).toContainText('Sent when an event of');
  await page.waitForTimeout(2_500);
  expect(await eventsOf(north)).toEqual([]);
  expect(await eventsOf('club')).toEqual([E2E_EVENT_NAME]);

  // The director opens the other event. Grid goes over to its club.
  await fetch(`${director.baseUrl}/active-event`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ id: second.id })
  });
  await expect(state(page)).toHaveText('Connected to North Whoops', { timeout: 20_000 });
  await expect(row(page, 'North Night')).toContainText('On the server', { timeout: 15_000 });
  await expect(row(page, 'North Night')).toContainText('Open');
  await expect(row(page, E2E_EVENT_NAME)).toContainText(
    `Sent when an event of ${server.club} is open.`
  );
  // Each club has its own event, and neither has the other's.
  expect(await eventsOf(north)).toEqual(['North Night']);
  expect(await eventsOf('club')).toEqual([E2E_EVENT_NAME]);
  // Both clubs are named as the server names them.
  await expect(page.getByTestId('link-club').nth(0)).toContainText(server.club);
  await expect(page.getByTestId('link-club').nth(1)).toContainText('North Whoops');

  // Removing a club is asked about first, and then its event goes nowhere.
  await page.getByRole('button', { name: 'Remove North Whoops' }).click();
  const sure = page.getByRole('dialog');
  await expect(sure).toContainText('1 event goes to this club');
  await sure.getByRole('button', { name: 'Remove' }).click();
  await expect(sure).toHaveCount(0);
  await expect(page.getByTestId('link-club')).toHaveCount(1);
  await expect(state(page)).toHaveText('Not sent');
  await expect(whereSent(page, 'North Night')).toHaveValue('');
  // What North holds stays there.
  expect(await eventsOf(north)).toEqual(['North Night']);
});

test('a server that is not there is said plainly, and racing is not held up', async ({
  page,
  director
}) => {
  await gotoPage(page, 'Server');
  // A port of this computer that nothing answers on.
  await linkOnPage(page, 'http://127.0.0.1:9', 'gfs_nobody-home', E2E_EVENT_NAME);
  await expect(state(page)).toHaveText('Not connected', { timeout: 15_000 });
  await expect(page.getByTestId('link-problem')).toContainText('could not be reached');
  await expect(row(page, E2E_EVENT_NAME)).not.toContainText('On the server');
  // The Director still answers.
  expect((await fetch(`${director.eventRoot}/state`)).ok).toBe(true);
});

test('a token the server does not take is said plainly, and a right one fixes it', async ({
  page
}) => {
  await gotoPage(page, 'Server');
  await linkOnPage(page, server.baseUrl, 'gfs_not-the-token', E2E_EVENT_NAME);
  await expect(page.getByTestId('link-problem')).toContainText('did not take the link token', {
    timeout: 15_000
  });
  // The director types the right token over it.
  await page.getByRole('button', { name: /^Change / }).click();
  await page.getByLabel('Link token').fill(server.token);
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(state(page)).toHaveText(`Connected to ${server.club}`, { timeout: 15_000 });
  await expect(page.getByTestId('link-problem')).toHaveCount(0);
});

test('what is wrong with the form is said before it is sent', async ({ page }) => {
  await gotoPage(page, 'Server');
  await page.getByRole('button', { name: 'Add a club' }).click();
  await page.getByLabel('Address').fill('grid.example.org');
  await expect(page.getByRole('alert')).toContainText('https://');
  await expect(page.getByRole('button', { name: 'Save' })).toBeDisabled();
  // The Director's own check: only a server on this computer may use plain http.
  await page.getByLabel('Address').fill('http://grid.example.org');
  await page.getByLabel('Link token').fill('gfs_x');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText(/Only a server on this same computer/)).toBeVisible();
  await expect(page.getByTestId('link-club')).toHaveCount(0);
  await expect(state(page)).toHaveText('Not sent');
});
