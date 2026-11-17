/**
 * Pilots signing themselves up for heats (docs/primitives.md §11.5), through real
 * browsers: the director's console on the Grid computer, and two pilots' phones that come
 * in by its network address.
 *
 * The phones are signed in once and never reloaded. What the director does has to show on
 * them by itself, and what a pilot does has to show on the director's sheet.
 */
import { createServer } from 'node:net';
import { networkInterfaces } from 'node:os';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test, type Page } from '@playwright/test';

import { type Director, startDirector } from '../test-harness/director.js';
import { openPage, pagesNav } from './nav.js';

const here = fileURLToPath(new URL('.', import.meta.url));
const dist = resolve(here, '..', 'apps', 'rd-console', 'dist');

const lan = Object.values(networkInterfaces())
  .flat()
  .find(
    (entry) =>
      entry && entry.family === 'IPv4' && !entry.internal && !entry.address.startsWith('169.254.')
  )?.address;

function freePort(): Promise<number> {
  return new Promise((done, reject) => {
    const srv = createServer();
    srv.on('error', reject);
    srv.listen(0, '0.0.0.0', () => {
      const address = srv.address();
      const found = typeof address === 'object' && address ? address.port : 0;
      srv.close(() => done(found));
    });
  });
}

let director: Director;
let address: string;
let event: string;
const codes: Record<string, string> = {};

async function direct<T = unknown>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(`${director.baseUrl}${path}`, {
    method,
    headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body)
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`${method} ${path} answered ${res.status}: ${text}`);
  return (text ? JSON.parse(text) : undefined) as T;
}
const command = (body: unknown) => direct('POST', `/events/${event}/commands`, body);

test.skip(lan === undefined, 'this computer has no network address to come in by');

test.beforeAll(async () => {
  director = await startDirector({ token: false, assets: dist });
  const ids: Record<string, string> = {};
  for (const callsign of ['Ace', 'Bee', 'Cat']) {
    ids[callsign] = (await direct<{ id: string }>('POST', '/pilots', { callsign })).id;
  }
  const open = (
    await direct<{ id: string }>('POST', '/classes', { name: 'Open', source: 'Custom' })
  ).id;
  event = (await direct<{ id: string }>('POST', '/events', { name: 'Friday Night Whoops' })).id;
  await direct('PUT', '/active-event', { id: event });
  await command({ Record: { RosterSet: { class: open, pilots: Object.values(ids) } } });
  await command({
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
  const access = await direct<{ pilots: { pilot: string; code: string }[] }>(
    'GET',
    `/events/${event}/access`
  );
  for (const [callsign, id] of Object.entries(ids)) {
    codes[callsign] = access.pilots.find((entry) => entry.pilot === id)!.code;
  }
  const port = await freePort();
  await direct('PUT', '/network', { enabled: true, port });
  await expect
    .poll(async () => (await direct<{ state: string }>('GET', '/network')).state)
    .toBe('On');
  address = `http://${lan}:${port}/`;
});

test.afterAll(async () => {
  await director?.stop();
});

/** A pilot's phone, signed in and on the Sign up tab. */
async function phoneOf(context: { newPage(): Promise<Page> }, callsign: string): Promise<Page> {
  const page = await context.newPage();
  await page.goto(`${address}#/join/${codes[callsign]}`);
  await expect(page.getByTestId('pilot-signed-in')).toHaveText(
    new RegExp(`Signed in as ${callsign}`),
    {
      timeout: 15_000
    }
  );
  return page;
}
const phoneHeat = (page: Page, n: number) =>
  page.getByRole('listitem', { name: `Qualifying Heat ${n}`, exact: true });

test('the director turns sign-up on, two pilots sign up on their phones, and every rule holds', async ({
  page,
  browser
}) => {
  test.setTimeout(180_000);
  const acePhone = await browser.newContext({ viewport: { width: 390, height: 844 } });
  const beePhone = await browser.newContext({ viewport: { width: 390, height: 844 } });
  try {
    // ── The director turns sign-up on ────────────────────────────────────────────────────
    await page.goto(director.baseUrl);
    await expect(pagesNav(page)).toBeVisible({ timeout: 15_000 });
    await openPage(page, 'Phases');
    await page.getByRole('button', { name: 'Set up pilot sign-up for Qualifying' }).click();
    const dialog = page.getByRole('dialog');
    await expect(dialog.getByRole('heading')).toHaveText('Pilot sign-up: Qualifying');
    // No timer is set up here, so the seats are the bench's. Keep the first three.
    for (const seat of [4, 5, 6, 7, 8]) {
      await dialog.getByRole('checkbox', { name: `Seat ${seat}`, exact: true }).uncheck();
    }
    await dialog.getByLabel('Runs for each pilot').fill('2');
    await dialog.getByRole('button', { name: 'Turn sign-up on' }).click();
    await expect(dialog).toHaveCount(0);

    const sheet = page.getByRole('table', { name: 'Heats of Qualifying' });
    const sheetRow = (n: number) => sheet.locator('tbody tr').nth(n - 1);
    await expect(sheet.locator('tbody tr')).toHaveCount(3);
    await expect(page.getByTestId('signup-open')).toHaveText('Closed to pilots');

    // ── A pilot's phone: the sheet is there, but closed ──────────────────────────────────
    const ace = await phoneOf(acePhone, 'Ace');
    await ace.getByRole('tab', { name: 'Sign up' }).click();
    await expect(ace.getByRole('region', { name: 'Sign-up for Qualifying' })).toBeVisible();
    await expect(phoneHeat(ace, 1)).toContainText('Sign-up is closed');
    await expect(ace.getByRole('button', { name: /^Take/ })).toHaveCount(0);

    // ── The director opens it: the phone gets its buttons by itself ──────────────────────
    await page.getByRole('button', { name: 'Open sign-up' }).click();
    await expect(page.getByTestId('signup-open')).toHaveText('Open to pilots');
    const takeOnAce = ace.getByRole('button', { name: 'Take Seat 1 in Qualifying Heat 1' });
    await expect(takeOnAce).toBeVisible({ timeout: 10_000 });
    await expect(ace.getByTestId('my-runs')).toHaveText('0 flown · 0 held · 2 left of 2');

    // ── Ace takes a seat: it shows on the director's sheet ───────────────────────────────
    await takeOnAce.click();
    await expect(ace.getByRole('button', { name: 'Leave Qualifying Heat 1' })).toBeVisible();
    await expect(ace.getByTestId('my-runs')).toHaveText('0 flown · 1 held · 1 left of 2');
    await expect(sheetRow(1)).toContainText('Ace', { timeout: 10_000 });

    // ── Bee: Ace's seat is not on offer; next free on that seat is the next heat ─────────
    const bee = await phoneOf(beePhone, 'Bee');
    await bee.getByRole('tab', { name: 'Sign up' }).click();
    await expect(phoneHeat(bee, 1)).toContainText('Ace');
    await expect(bee.getByRole('button', { name: 'Take Seat 1 in Qualifying Heat 1' })).toHaveCount(
      0
    );
    await bee.getByRole('button', { name: 'Next free spot on Seat 1' }).click();
    await expect(bee.getByRole('button', { name: 'Leave Qualifying Heat 2' })).toBeVisible();
    await bee.getByRole('button', { name: 'Take Seat 2 in Qualifying Heat 1' }).click();
    await expect(bee.getByTestId('my-runs')).toHaveText('0 flown · 2 held · 0 left of 2');
    // No runs left: nothing more to take, and it says why.
    await expect(bee.getByRole('button', { name: /^Take/ })).toHaveCount(0);
    await expect(phoneHeat(bee, 3)).toContainText('No runs left');
    // Ace's phone shows Bee beside them, without being touched.
    await expect(phoneHeat(ace, 1)).toContainText('Bee', { timeout: 10_000 });

    // ── The director adds Cat: heat 1 is full, so a fourth heat appears everywhere ───────
    await page.getByRole('button', { name: 'Add a pilot to Seat 3 of Qualifying Heat 1' }).click();
    await page.getByRole('dialog').getByRole('button', { name: 'Cat' }).click();
    await expect(sheetRow(1)).toContainText('Cat');
    await expect(sheetRow(1)).toContainText('Full');
    await expect(sheet.locator('tbody tr')).toHaveCount(4);
    await expect(phoneHeat(ace, 4)).toBeVisible({ timeout: 10_000 });
    await expect(phoneHeat(ace, 1)).toContainText('Full');

    // ── Bee gives a spot up and gets a run back ──────────────────────────────────────────
    await bee.getByRole('button', { name: 'Leave Qualifying Heat 2' }).click();
    await expect(bee.getByTestId('my-runs')).toHaveText('0 flown · 1 held · 1 left of 2');
    await expect(sheetRow(2)).not.toContainText('Bee', { timeout: 10_000 });

    // ── The director closes heat 1 to pilots: nobody can leave it from a phone ───────────
    await page.getByRole('button', { name: 'Close Qualifying Heat 1 to pilots' }).click();
    await expect(sheetRow(1)).toContainText('Closed');
    await expect(ace.getByRole('button', { name: 'Leave Qualifying Heat 1' })).toHaveCount(0, {
      timeout: 10_000
    });
    // A closed heat is off the pilot's sheet: only heats they can still change are on it.
    await expect(phoneHeat(ace, 1)).toHaveCount(0);
    await expect(phoneHeat(ace, 2)).toBeVisible();
    // Asking the Director anyway gets its reason, and changes nothing.
    const sneak = await ace.request.post(`${address}public/pilot/signup/leave`, {
      data: { heat: 'quali/heat-1' }
    });
    expect(sneak.status()).toBe(409);
    expect(((await sneak.json()) as { message: string }).message).toBe(
      'Qualifying Heat 1 is closed to changes from a phone. Ask the director'
    );

    // ── The director drops Cat and says it counts ────────────────────────────────────────
    await page.getByRole('button', { name: 'Drop Cat from Qualifying Heat 1' }).click();
    await page.getByRole('dialog').getByRole('button', { name: 'Counts as a run' }).click();
    const runs = page.getByRole('table', { name: 'Runs of each pilot in Qualifying' });
    const runRow = (name: string) => runs.locator('tbody tr').filter({ hasText: name });
    await expect(runRow('Cat').getByRole('cell').nth(0)).toHaveText('1');
    await expect(sheetRow(1)).not.toContainText('Cat');

    // ── Heat 1 is flown: it is a run for Ace and Bee, and nobody can change it ───────────
    await command({ StartRace: { heat: 'quali/heat-1' } });
    await expect(sheetRow(1)).toContainText('On now', { timeout: 10_000 });
    await expect(phoneHeat(ace, 1)).toHaveCount(0);
    await command({ StopRace: { race: 'quali/heat-1#1' } });
    await command({ SaveRace: { race: 'quali/heat-1#1' } });
    await expect(ace.getByTestId('my-runs')).toHaveText('1 flown · 0 held · 1 left of 2', {
      timeout: 10_000
    });
    // The flown heat stays off the pilot's sheet, and on the director's.
    await expect(phoneHeat(ace, 1)).toHaveCount(0);
    await expect(sheetRow(1)).toContainText('Flown');
    await expect(sheetRow(1).getByRole('button')).toHaveCount(0);
    await expect(runRow('Ace').getByRole('cell').nth(0)).toHaveText('1');

    // ── One more run for everyone, then one more for Ace alone ───────────────────────────
    await page.getByRole('button', { name: 'One more run for everyone' }).click();
    await expect(ace.getByTestId('my-runs')).toHaveText('1 flown · 0 held · 2 left of 3', {
      timeout: 10_000
    });
    await page.getByRole('button', { name: 'One more run for Ace' }).click();
    await expect(ace.getByTestId('my-runs')).toHaveText('1 flown · 0 held · 3 left of 4', {
      timeout: 10_000
    });
    await expect(bee.getByTestId('my-runs')).toHaveText('1 flown · 0 held · 2 left of 3');

    // ── Ace signs up again, and their own tab shows the heat they are in ─────────────────
    await ace.getByRole('button', { name: 'Take Seat 2 in Qualifying Heat 2' }).click();
    await ace.getByRole('tab', { name: /^Me/ }).click();
    await expect(ace.getByRole('list', { name: 'My heats' })).toContainText('Qualifying Heat 2');
    await expect(ace.getByRole('list', { name: 'My heats' })).toContainText('Seat 2');
    await expect(ace.getByTestId('pilot-alert')).toContainText('You are next');

    // ── A phone that is signed out has no sign-up ─────────────────────────────────────────
    await bee.getByRole('button', { name: 'Sign out' }).click();
    // Sign-up is for pilots: the tab and the sheet go, and the page is back on the race.
    await expect(bee.getByRole('tab', { name: 'Sign up' })).toHaveCount(0);
    await expect(bee.getByRole('region', { name: 'Sign-up for Qualifying' })).toHaveCount(0);
    await expect(bee.getByRole('button', { name: 'Pilot', exact: true })).toBeVisible();
    const refused = await bee.request.post(`${address}public/pilot/signup/take`, {
      data: { heat: 'quali/heat-3', seat: { adapter: 'bench', seat: 'seat-1' } }
    });
    expect(refused.status()).toBe(401);
  } finally {
    await acePhone.close();
    await beePhone.close();
  }
});
