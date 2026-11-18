/**
 * What a signed-in pilot sees on their phone (docs/primitives.md §11.2), in a real browser.
 *
 * The phone is signed in once and never reloaded. As the director runs the heats, the
 * alert on it has to move by itself from "1 heat away" to "next" to "flying now", and the
 * pilot's own races have to appear lap by lap. One heat is on a timer, so the phone also
 * has to show which channel that seat is on.
 */
import { createServer as createHttpServer, type Server } from 'node:http';
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
let timer: Server;
let timerAsked = 0;
let address: string;
let event: string;
let ace: string;
let aceCode: string;

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
const crossing = (seat: string) =>
  direct('POST', `/events/${event}/passes`, { adapter: 'bench', seat, at: Date.now() * 1000 });

const FORMAT = {
  name: 'One race each',
  race: { end: 'Open', staging_micros: 0, first_lap: 'HoleShot' },
  completion: { SavedRaces: 1 },
  ranking: { sort: ['LastFinish'] }
};
const heat = (id: string, label: string, lineup: unknown[]) => ({
  Record: {
    PhaseDefined: { def: { id, label, kind: { Heat: { lineup } }, format: FORMAT } }
  }
});

test.skip(lan === undefined, 'this computer has no network address to come in by');

test.beforeAll(async () => {
  // A stand-in for an OFT timer's settings: seat 0 is on R1, seat 1 on R2.
  const timerPort = await freePort();
  timer = createHttpServer((req, res) => {
    if (req.url === '/oft/v1/config') {
      timerAsked += 1;
      res.setHeader('Content-Type', 'application/json');
      res.end(
        JSON.stringify({
          seats: [
            { seat: 0, freq_mhz: 5658, enabled: true },
            { seat: 1, freq_mhz: 5695, enabled: true }
          ],
          threshold: { mode: 'auto', snr_enter: 10, hysteresis: 3 },
          min_lap_ms: 2000
        })
      );
      return;
    }
    res.statusCode = 404;
    res.end();
  });
  await new Promise<void>((done) => timer.listen(timerPort, '127.0.0.1', done));

  director = await startDirector({ token: false, assets: dist });
  ace = (await direct<{ id: string }>('POST', '/pilots', { callsign: 'Ace' })).id;
  const bee = (await direct<{ id: string }>('POST', '/pilots', { callsign: 'Bee' })).id;
  const cat = (await direct<{ id: string }>('POST', '/pilots', { callsign: 'Cat' })).id;
  const open = (
    await direct<{ id: string }>('POST', '/classes', { name: 'Open', source: 'Custom' })
  ).id;
  const gate = (
    await direct<{ id: string }>('POST', '/timers', {
      name: 'Gate',
      kind: 'Oft',
      address: `127.0.0.1:${timerPort}`
    })
  ).id;
  event = (await direct<{ id: string }>('POST', '/events', { name: 'Friday Night Whoops' })).id;
  await direct('PUT', '/active-event', { id: event });
  await command({ Record: { RosterSet: { class: open, pilots: [ace, bee, cat] } } });
  const bench = (seat: number, pilot: string) => ({
    adapter: 'bench',
    seat: `seat-${seat}`,
    source: { Pilot: pilot }
  });
  await command(heat('heat-1', 'Heat 1', [bench(1, bee), bench(2, cat)]));
  await command(heat('heat-2', 'Heat 2', [bench(1, ace), bench(2, bee)]));
  await command(
    heat('heat-3', 'Heat 3', [
      { adapter: gate, seat: '0', source: { Pilot: ace } },
      { adapter: gate, seat: '1', source: { Pilot: cat } }
    ])
  );
  // The test below counts on the heats being queued in this order.
  const state = await direct<{ queue: string[] }>('GET', `/events/${event}/state`);
  expect(state.queue).toEqual(['heat-1', 'heat-2', 'heat-3']);

  const access = await direct<{ pilots: { pilot: string; code: string }[] }>(
    'GET',
    `/events/${event}/access`
  );
  aceCode = access.pilots.find((entry) => entry.pilot === ace)!.code;

  const port = await freePort();
  await direct('PUT', '/network/public', { enabled: true, port });
  await expect
    .poll(async () => (await direct<{ public: { state: string } }>('GET', '/network')).public.state)
    .toBe('On');
  address = `http://${lan}:${port}/`;
});

test.afterAll(async () => {
  await director?.stop();
  await new Promise<void>((done) => timer?.close(() => done()));
});

test("a pilot's phone counts them down to their race and then shows their laps", async ({
  browser
}) => {
  test.setTimeout(120_000);
  const phone = await browser.newContext({ viewport: { width: 390, height: 844 } });
  const page = await phone.newPage();
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(String(error)));
  // Count the buzzes the page asks the phone for.
  await page.addInitScript(() => {
    const w = window as unknown as { buzzes: number[][] };
    w.buzzes = [];
    navigator.vibrate = ((pattern: number | number[]) => {
      w.buzzes.push(Array.isArray(pattern) ? pattern : [pattern]);
      return true;
    }) as typeof navigator.vibrate;
  });
  const buzzes = () => page.evaluate(() => (window as unknown as { buzzes: number[][] }).buzzes);

  try {
    // ── Sign in by the QR code's link ────────────────────────────────────────────────────
    await page.goto(`${address}#/join/${aceCode}`);
    await expect(page.getByTestId('pilot-signed-in')).toHaveText(/Signed in as Ace/, {
      timeout: 15_000
    });
    const me = page.getByRole('tab', { name: /^Me/ });
    await expect(me).toHaveAttribute('aria-selected', 'true');
    await expect(me).toContainText('2');

    // ── Two heats listed; the nearer one is one heat away ────────────────────────────────
    const alert = page.getByTestId('pilot-alert');
    await expect(alert).toContainText('You are 1 heat away');
    await expect(alert).toContainText('Heat 2 · Seat 1');
    const mine = page.getByRole('list', { name: 'My heats' }).getByRole('listitem');
    await expect(mine).toHaveCount(2);
    await expect(mine.nth(0)).toContainText('1 heat away');
    await expect(mine.nth(0)).toContainText('Heat 2');
    // The heat on the timer says which channel the pilot flies on. The bench heat says none.
    await expect(mine.nth(1)).toContainText('Heat 3');
    await expect(mine.nth(1)).toContainText('R1 · 5658 MHz', { timeout: 30_000 });
    await expect(mine.nth(0)).not.toContainText('MHz');
    await expect(page.getByTestId('no-races')).toBeVisible();
    await expect.poll(async () => (await buzzes()).length).toBe(1);

    // Everyone sees the channels of the seats coming up.
    await page.getByRole('tab', { name: /Up next/ }).click();
    const heat3 = page.getByRole('region', { name: 'Up next' }).getByRole('listitem', {
      name: 'Heat 3'
    });
    // The channel is part of the seat's name, wherever a seat is named.
    await expect(heat3.locator('li').filter({ hasText: 'Cat' })).toContainText(/Seat \d · R2/);
    await me.click();

    // ── Heat 1 runs: still one heat away. It is saved: next. ─────────────────────────────
    await command({ StartRace: { heat: 'heat-1' } });
    await expect(alert).toContainText('You are 1 heat away', { timeout: 10_000 });
    await command({ StopRace: { race: 'heat-1#1' } });
    await command({ SaveRace: { race: 'heat-1#1' } });
    await expect(alert).toContainText('You are next', { timeout: 10_000 });
    await expect(alert).toHaveAttribute('data-nearness', 'next');
    await expect(mine.nth(0)).toContainText('Next');
    await expect.poll(async () => (await buzzes()).length).toBe(2);

    // ── Heat 2 starts: flying now ────────────────────────────────────────────────────────
    await command({ StartRace: { heat: 'heat-2' } });
    await expect(alert).toContainText('You are flying now', { timeout: 10_000 });
    await expect(mine.nth(0)).toContainText('On now');
    await expect.poll(async () => (await buzzes()).length).toBe(3);
    // On the race tab, the pilot's own row is the marked one.
    await page.getByRole('tab', { name: 'Current race' }).click();
    const race = page.getByRole('table', { name: 'Pilots in this race' });
    await expect(race.locator('tbody tr.me')).toHaveCount(1);
    await expect(race.locator('tbody tr.me')).toContainText('Ace');

    // ── Ace flies three laps; the race is saved ──────────────────────────────────────────
    await crossing('seat-1');
    for (let lap = 0; lap < 3; lap += 1) {
      await page.waitForTimeout(1100);
      await crossing('seat-1');
    }
    await expect(race.locator('tbody tr.me').getByRole('cell').nth(1)).toHaveText('3', {
      timeout: 10_000
    });
    await command({ StopRace: { race: 'heat-2#1' } });
    await command({ SaveRace: { race: 'heat-2#1' } });

    // ── My races: this one, lap by lap ───────────────────────────────────────────────────
    await me.click();
    const flown = page.getByRole('list', { name: 'My races' });
    await expect(flown).toContainText('Heat 2 · race 1', { timeout: 10_000 });
    const laps = page.getByRole('list', { name: 'Laps of Heat 2 · race 1' }).getByRole('listitem');
    await expect(laps).toHaveCount(3);
    for (let lap = 0; lap < 3; lap += 1) {
      await expect(laps.nth(lap)).toHaveText(new RegExp(`^${lap + 1}\\s*\\d+\\.\\d{3}$`));
    }
    await expect(flown.locator('li.best')).toHaveCount(1);
    await expect(flown).toContainText('1st');
    await expect(flown).toContainText(/Best \d+\.\d{3}/);

    // ── And the alert has moved on to the heat on the timer, channel and all ─────────────
    await expect(alert).toContainText('You are next');
    await expect(alert).toContainText('Heat 3 · Seat 1 · R1 · 5658 MHz');
    await expect(mine).toHaveCount(1);
    await expect.poll(async () => (await buzzes()).length).toBe(4);

    // ── However long the phone watched, the timer was asked only a few times ─────────────
    expect(timerAsked).toBeGreaterThanOrEqual(1);
    expect(timerAsked).toBeLessThanOrEqual(8);

    // ── Signing out: a spectator again ───────────────────────────────────────────────────
    await page.getByRole('button', { name: 'Sign out' }).click();
    await expect(page.getByRole('tab', { name: /^Me/ })).toHaveCount(0);
    await expect(alert).toHaveCount(0);
    expect(errors).toEqual([]);
  } finally {
    await phone.close();
  }
});
