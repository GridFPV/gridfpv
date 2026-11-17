/**
 * A spectator on a phone, watching a race the director runs (docs/primitives.md §11.2).
 *
 * The phone opens Grid by the computer's network address and is never reloaded. Everything
 * it shows after that has to arrive by itself: the race starting, each lap, the result,
 * a pilot added to the event, and the director opening another event.
 *
 * The director's side is driven over the API, so the test is about the phone.
 */
import { createServer } from 'node:net';
import { networkInterfaces } from 'node:os';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test } from '@playwright/test';

import { type Director, startDirector } from '../test-harness/director.js';

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

/** A write by the director, over the API from this computer. */
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

const FORMAT = {
  name: 'First to 3 laps',
  race: { end: { FirstToLaps: 3 }, staging_micros: 2_000_000, first_lap: 'HoleShot' },
  completion: { SavedRaces: 1 },
  ranking: { sort: ['LastFinish'] }
};

let event: string;
let ace: string;
let bee: string;

async function command(body: unknown): Promise<void> {
  await direct('POST', `/events/${event}/commands`, body);
}
async function crossing(seat: string): Promise<void> {
  await direct('POST', `/events/${event}/passes`, {
    adapter: 'bench',
    seat,
    at: Date.now() * 1000
  });
}
function heat(id: string, label: string, pilots: string[]) {
  return {
    Record: {
      PhaseDefined: {
        def: {
          id,
          label,
          kind: {
            Heat: {
              lineup: pilots.map((pilot, index) => ({
                adapter: 'bench',
                seat: `seat-${index + 1}`,
                source: { Pilot: pilot }
              }))
            }
          },
          format: FORMAT
        }
      }
    }
  };
}

test.skip(lan === undefined, 'this computer has no network address to come in by');

test.beforeAll(async () => {
  director = await startDirector({ token: false, assets: dist });
  ace = (await direct<{ id: string }>('POST', '/pilots', { callsign: 'Ace', color: '#FF3344' })).id;
  bee = (await direct<{ id: string }>('POST', '/pilots', { callsign: 'Bee' })).id;
  event = (await direct<{ id: string }>('POST', '/events', { name: 'Friday Night Whoops' })).id;
  await direct('PUT', '/active-event', { id: event });
  await command(heat('heat-1', 'Qualifying Heat 1', [ace, bee]));
  await command(heat('heat-2', 'Qualifying Heat 2', [bee]));
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

test('a spectator watches a race from start to result without touching the page', async ({
  browser
}) => {
  test.setTimeout(120_000);
  const phone = await browser.newContext({ viewport: { width: 390, height: 844 } });
  const page = await phone.newPage();
  const errors: string[] = [];
  const asked = new Set<string>();
  page.on('pageerror', (error) => errors.push(String(error)));
  page.on('console', (message) => {
    if (message.type() === 'error') errors.push(message.text());
  });
  page.on('request', (request) => asked.add(new URL(request.url()).pathname));

  try {
    await page.goto(address);
    await expect(page.getByTestId('visitor-event')).toHaveText('Friday Night Whoops', {
      timeout: 15_000
    });

    // ── Before any race ──────────────────────────────────────────────────────────────────
    await expect(page.getByTestId('no-race')).toHaveText('No race yet.');
    const upNext = page.getByRole('tab', { name: /Up next/ });
    await expect(upNext).toContainText('2');
    await upNext.click();
    const coming = page.getByRole('region', { name: 'Up next' });
    await expect(coming.getByRole('listitem', { name: 'Qualifying Heat 1' })).toContainText('Ace');
    await expect(coming.getByRole('listitem', { name: 'Qualifying Heat 1' })).toContainText('Bee');
    await expect(coming.getByRole('listitem', { name: 'Qualifying Heat 2' })).toContainText('Bee');
    await page.getByRole('tab', { name: 'Last race' }).click();

    // ── The director starts heat 1: the phone shows it by itself ─────────────────────────
    await command({ StartRace: { heat: 'heat-1' } });
    await expect(page.getByRole('tab', { name: 'On now' })).toBeVisible({ timeout: 10_000 });
    await expect(page.getByTestId('race-heat')).toHaveText('Qualifying Heat 1');
    await expect(page.getByTestId('race-word')).toHaveText('Starting');
    await expect(page.getByTestId('race-clock')).toBeVisible();
    await expect(page.getByTestId('race-word')).toHaveText('Racing', { timeout: 15_000 });
    // The clock moves.
    const before = await page.getByTestId('race-clock').textContent();
    await expect
      .poll(async () => page.getByTestId('race-clock').textContent(), { timeout: 5000 })
      .not.toBe(before);
    // The heat that is on is no longer "next".
    await expect(upNext).toContainText('1');

    // ── Laps arrive as they are flown ────────────────────────────────────────────────────
    const table = page.getByRole('table', { name: 'Pilots in this race' });
    // Body rows only: the heading row's "Place" would match a search for "Ace".
    const row = (name: string) => table.locator('tbody tr').filter({ hasText: name });
    const laps = (name: string) => row(name).getByRole('cell').nth(1);
    await expect(laps('Ace')).toHaveText('0');
    await crossing('seat-1'); // Ace's hole shot
    await crossing('seat-2'); // Bee's hole shot
    await page.waitForTimeout(1200);
    await crossing('seat-1');
    await expect(laps('Ace')).toHaveText('1', { timeout: 10_000 });
    await expect(laps('Bee')).toHaveText('0');
    await expect(row('Ace').getByRole('cell').first()).toHaveText('1st');
    await page.waitForTimeout(1200);
    await crossing('seat-2');
    await expect(laps('Bee')).toHaveText('1', { timeout: 10_000 });
    await page.waitForTimeout(1200);
    await crossing('seat-1');
    await expect(laps('Ace')).toHaveText('2', { timeout: 10_000 });
    // A lap time is on the row, to the thousandth.
    await expect(row('Ace').getByRole('cell').nth(2)).toHaveText(/^\d+\.\d{3}$/);
    await page.waitForTimeout(1200);
    await crossing('seat-1');
    await expect(laps('Ace')).toHaveText('3', { timeout: 10_000 });

    // ── The director stops the race and saves it ─────────────────────────────────────────
    const state = await direct<{ races: { state: string }[] }>('GET', `/events/${event}/state`);
    if (state.races[0].state === 'Running') await command({ StopRace: { race: 'heat-1#1' } });
    await expect(page.getByTestId('race-word')).toHaveText('Finished', { timeout: 15_000 });
    await command({ SaveRace: { race: 'heat-1#1', note: 'a note only the director sees' } });
    await expect(page.getByRole('tab', { name: 'Last race' })).toBeVisible({ timeout: 10_000 });
    await expect(page.getByTestId('race-word')).toHaveText('Result');
    await expect(page.getByTestId('race-clock')).toHaveCount(0);
    await expect(page.getByText('a note only the director sees')).toHaveCount(0);

    // ── Results ──────────────────────────────────────────────────────────────────────────
    await page.getByRole('tab', { name: 'Results' }).click();
    const results = page.getByRole('table', { name: 'Results' });
    await expect(results.getByRole('rowheader').first()).toContainText('Ace');
    await expect(results.locator('tbody tr').filter({ hasText: 'Ace' })).toContainText('1st');
    await expect(results.locator('tbody tr').filter({ hasText: 'Bee' })).toContainText('2nd');
    // Pick the heat, then open its race's lap times large.
    await page.getByLabel('Show', { exact: true }).selectOption({ label: 'Qualifying Heat 1' });
    await expect(results).toHaveCount(0);
    await page.getByRole('button', { name: /^Lap times of Qualifying Heat 1/ }).click();
    const lapTimes = page.getByRole('dialog');
    await expect(lapTimes.getByRole('heading')).toContainText('Lap times: Qualifying Heat 1');
    await expect(lapTimes.getByRole('region', { name: 'Laps of Ace' })).toContainText('1st');
    await expect(lapTimes.getByRole('region', { name: 'Laps of Bee' })).toBeVisible();
    await lapTimes.getByRole('button', { name: 'Close' }).click();
    await expect(lapTimes).toHaveCount(0);
    await page.getByLabel('Show', { exact: true }).selectOption({ label: 'Overall' });
    await expect(results).toBeVisible();

    // ── A pilot the phone has never heard of joins the event ─────────────────────────────
    const cat = (await direct<{ id: string }>('POST', '/pilots', { callsign: 'Cat' })).id;
    await command(heat('heat-3', 'Qualifying Heat 3', [cat]));
    await upNext.click();
    const heat3 = coming.getByRole('listitem', { name: 'Qualifying Heat 3' });
    await expect(heat3).toBeVisible({ timeout: 10_000 });
    await expect(heat3).toContainText('Cat', { timeout: 10_000 });
    await expect(coming).not.toContainText('Unknown pilot');

    // ── The director opens another event: the phone follows ──────────────────────────────
    const other = (await direct<{ id: string }>('POST', '/events', { name: 'Sunday Finals' })).id;
    await direct('PUT', '/active-event', { id: other });
    await expect(page.getByTestId('visitor-event')).toHaveText('Sunday Finals', {
      timeout: 15_000
    });
    await page.getByRole('tab', { name: 'Last race' }).click();
    await expect(page.getByTestId('no-race')).toBeVisible({ timeout: 10_000 });
    await expect(page.getByText('Qualifying Heat 1')).toHaveCount(0);

    // ── And closes it ────────────────────────────────────────────────────────────────────
    await direct('DELETE', `/events/${other}`);
    await expect(page.getByTestId('visitor-event')).toHaveText('No event is open', {
      timeout: 15_000
    });

    // ── The phone asked only for public things, and nothing went wrong on it ─────────────
    const api = [...asked].filter((path) => !path.startsWith('/assets/') && path !== '/');
    for (const path of api) {
      expect(path.startsWith('/public/'), `the phone asked for ${path}`).toBe(true);
    }
    expect(api).toContain('/public/event/changes');
    // Reading an event that has just been closed fails once, by design; nothing else may.
    expect(errors.filter((line) => !line.includes('404'))).toEqual([]);

    // ── Grid goes away: the phone says it is not connected, and keeps what it has ────────
    await direct('PUT', '/network', { enabled: false });
    await expect(page.getByTestId('visitor-offline')).toBeVisible({ timeout: 20_000 });
    await expect(page.getByTestId('visitor-event')).toBeVisible();
  } finally {
    await phone.close();
  }
});
