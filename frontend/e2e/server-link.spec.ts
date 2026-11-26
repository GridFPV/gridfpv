/**
 * The link to a remote server, through a real browser (docs/primitives.md §13).
 *
 * The director sets the link on the Server page of the real console. The Director is the
 * real `gridfpv` binary and the server is the real `gridfpv-server` binary, so what the
 * page says is on the server is checked by asking the server.
 */
import type { Page } from '@playwright/test';

import { startServer, type RemoteServer } from '../test-harness/server.js';
import { gotoPage } from './nav.js';
import { E2E_EVENT_NAME, expect, test } from './observability.js';

let server: RemoteServer;

test.beforeAll(async () => {
  server = await startServer();
});
test.afterAll(async () => {
  await server?.stop();
});
// The Director is shared by every spec in the worker: leave it pushing to nothing.
test.afterEach(async ({ director }) => {
  await fetch(`${director.baseUrl}/server-link`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ url: '', token: '', on: false })
  });
});

const state = (page: Page) => page.getByTestId('link-state');
const power = (page: Page) => page.getByRole('switch', { name: 'Send events to the server' });

async function setLink(page: Page, url: string, token: string): Promise<void> {
  await page.getByLabel('Address').fill(url);
  await page.getByLabel('Link token').fill(token);
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(state(page)).toHaveText('Off');
}

test('the Server page is under Setup, and starts with nothing set', async ({ page }) => {
  await gotoPage(page, 'Server');
  await expect.poll(() => new URL(page.url()).hash).toBe('#/server');
  await expect(state(page)).toHaveText('Not set');
  await expect(power(page)).not.toBeChecked();
  await expect(page.getByRole('button', { name: 'Save' })).toHaveCount(0);
});

test('an event goes to the server once the link is set and switched on', async ({
  page,
  director
}) => {
  await gotoPage(page, 'Server');
  await setLink(page, server.baseUrl, server.token);
  // Saved, and the token is not on the page any more.
  await expect(page.getByLabel('Link token')).toHaveValue('');
  expect(await page.content()).not.toContain(server.token);
  // Not switched on: nothing has been sent.
  const before = await fetch(`${server.baseUrl}/api/clubs/club/events`);
  expect(await before.json()).toEqual([]);

  await power(page).click();
  await expect(state(page)).toHaveText(`Connected to ${server.club}`, { timeout: 15_000 });
  const row = page.getByTestId('link-event').filter({ hasText: E2E_EVENT_NAME });
  await expect(row).toContainText('On the server', { timeout: 15_000 });

  // The server holds it, and shows what the Director's own public page shows.
  const list = (await (await fetch(`${server.baseUrl}/api/clubs/club/events`)).json()) as {
    id: string;
    name: string;
  }[];
  expect(list.map((event) => event.name)).toContain(E2E_EVENT_NAME);
  const read = async (base: string): Promise<{ cursor: number; phases: unknown[] }> =>
    (await fetch(`${base}/event/state`)).json() as Promise<{ cursor: number; phases: unknown[] }>;
  await fetch(`${director.baseUrl}/active-event`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ id: director.event })
  });
  await expect
    .poll(async () => {
      const theirs = await read(`${server.baseUrl}/api/clubs/club/events/${director.event}/public`);
      const ours = await read(`${director.baseUrl}/public`);
      return theirs.cursor === ours.cursor && theirs.phases.length === ours.phases.length;
    })
    .toBe(true);

  // Switched off, it stops, and says so.
  await power(page).click();
  await expect(state(page)).toHaveText('Off');
  await expect(page.getByTestId('link-event')).toHaveCount(0);
});

test('a server that is not there is said plainly, and racing is not held up', async ({
  page,
  director
}) => {
  await gotoPage(page, 'Server');
  // A port of this computer that nothing answers on.
  await setLink(page, 'http://127.0.0.1:9', 'gfs_nobody-home');
  await power(page).click();
  await expect(state(page)).toHaveText('Not connected', { timeout: 15_000 });
  await expect(page.getByTestId('link-problem')).toContainText('could not be reached');
  // The Director still answers.
  expect((await fetch(`${director.eventRoot}/state`)).ok).toBe(true);
});

test('a token the server does not take is said plainly', async ({ page }) => {
  await gotoPage(page, 'Server');
  await setLink(page, server.baseUrl, 'gfs_not-the-token');
  await power(page).click();
  await expect(page.getByTestId('link-problem')).toContainText('did not take the link token', {
    timeout: 15_000
  });
});

test('what is wrong with the form is said before it is sent', async ({ page }) => {
  await gotoPage(page, 'Server');
  await page.getByLabel('Address').fill('grid.example.org');
  await expect(page.getByRole('alert')).toContainText('https://');
  await expect(page.getByRole('button', { name: 'Save' })).toBeDisabled();
  // The Director's own check: only a server on this computer may use plain http.
  await page.getByLabel('Address').fill('http://grid.example.org');
  await page.getByLabel('Link token').fill('gfs_x');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText(/Only a server on this same computer/)).toBeVisible();
  await expect(state(page)).toHaveText('Not set');
});
