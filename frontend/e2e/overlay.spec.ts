/**
 * The race overlay, beside a console that is running a heat.
 *
 * One page is the console: a person adds a heat and runs it with the Run view's bench
 * "Crossing" buttons. A second page is the overlay as OBS would load it (`#/overlay/race`
 * at 1920x1080). It is never clicked. The spec checks that it shows the heat's name, the
 * callsigns, lap counts that go up as crossings are made, and the saved result; that it is
 * bare and see-through; that no wire handle is on it; and that it follows the Director to
 * another event by itself.
 *
 * Nothing is mocked: real commands, real passes, the real change feed, the real
 * `GET /active-event`.
 *
 * The worker's Director is shared, so every name here is unique and the spec deletes what it
 * made and puts the active event back.
 */
import { mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { Locator, Page } from '@playwright/test';
import { heatOnTimer } from './nav.js';
import { expect, test } from './observability.js';

const STAMP = Date.now();
const EVENT = `Overlay Night ${STAMP}`;
const SECOND_EVENT = `Overlay Sunday ${STAMP}`;
const HEAT = `Overlay Heat ${STAMP}`;
const ACE = `OV-Ace-${STAMP}`;
const BEE = `OV-Bee-${STAMP}`;

/** Where the mid-race picture goes: with the other run artifacts, never in the source tree. */
const ARTIFACTS = resolve(fileURLToPath(new URL('.', import.meta.url)), '..', 'e2e-artifacts');

/** One pilot's row on the overlay, and one number in it. */
const overlayRow = (overlay: Page, callsign: string): Locator =>
  overlay.getByRole('row', { name: callsign });
const stat = (row: Locator, name: string): Locator => row.locator(`[data-stat="${name}"]`);

async function createPilot(page: Page, callsign: string): Promise<string> {
  const res = await page.request.post('/pilots', { data: { callsign } });
  expect(res.ok()).toBe(true);
  return ((await res.json()) as { id: string }).id;
}

async function createEvent(page: Page, name: string): Promise<string> {
  const res = await page.request.post('/events', { data: { name } });
  expect(res.ok()).toBe(true);
  return ((await res.json()) as { id: string }).id;
}

test('the overlay follows a heat as it is raced, then follows the Director to another event', async ({
  page
}) => {
  const before = (await (await page.request.get('/active-event')).json()) as {
    event: { id: string } | null;
  };
  const made: { events: string[]; pilots: string[] } = { events: [], pilots: [] };
  const overlay = await page.context().newPage();
  const overlayErrors: string[] = [];
  overlay.on('pageerror', (error) => overlayErrors.push(String(error)));

  try {
    // ── Two pilots and an active event, set up through the API ───────────────────────────
    made.pilots.push(await createPilot(page, ACE), await createPilot(page, BEE));
    const eventId = await createEvent(page, EVENT);
    made.events.push(eventId);
    expect((await page.request.put('/active-event', { data: { id: eventId } })).ok()).toBe(true);

    // ── The overlay, as OBS loads it. With no race on it is down to its idle mark. ───────
    await overlay.setViewportSize({ width: 1920, height: 1080 });
    await overlay.goto('/#/overlay/race');
    const region = overlay.getByRole('region', { name: 'Race overlay' });
    await expect(region).toBeVisible({ timeout: 15_000 });
    await expect(overlay.getByTestId('overlay-idle')).toHaveText(EVENT, { timeout: 15_000 });
    await expect(overlay.getByRole('timer')).toHaveCount(0);

    // Bare and see-through: no shell around it, and nothing painted behind it.
    await expect(overlay.getByRole('navigation')).toHaveCount(0);
    await expect(overlay.getByRole('button')).toHaveCount(0);
    await expect(overlay.getByRole('dialog')).toHaveCount(0);
    const backgrounds = await overlay.evaluate(() => [
      getComputedStyle(document.documentElement).backgroundColor,
      getComputedStyle(document.body).backgroundColor,
      getComputedStyle(document.body, '::before').display
    ]);
    expect(backgrounds).toEqual(['rgba(0, 0, 0, 0)', 'rgba(0, 0, 0, 0)', 'none']);

    // ── The console: the event's Phases page, add a heat, open it on the Run page ────────
    await page.goto('/#/event/phases');
    await expect(page.getByRole('heading', { name: 'Phases', level: 1 })).toBeVisible({
      timeout: 15_000
    });
    await expect(page.locator('header a.event')).toContainText(EVENT);
    await page.getByRole('button', { name: '+ Add heat' }).click();
    const form = page.getByRole('form', { name: 'Add a heat' });
    await form.getByLabel('Heat name').fill(HEAT);
    await form.getByRole('button', { name: ACE, exact: true }).click();
    await form.getByRole('button', { name: BEE, exact: true }).click();
    await form.getByLabel('Format').selectOption({ label: 'First to 3 laps' });
    await page.getByRole('button', { name: 'Add heat', exact: true }).click();
    await expect(form).toBeHidden({ timeout: 15_000 });
    await page.getByRole('button', { name: `Run ${HEAT}`, exact: true }).click();
    await expect(heatOnTimer(page)).toHaveText(HEAT);

    // ── Start. The overlay names the heat and counts down, with both pilots listed. ──────
    await page.getByRole('button', { name: 'Start', exact: true }).click();
    await expect(overlay.getByTestId('overlay-heat')).toHaveText(HEAT, { timeout: 15_000 });
    await expect(overlay.getByTestId('overlay-state')).toHaveText('Starting');
    await expect(overlay.getByRole('timer')).toHaveAttribute('data-mode', 'remaining');
    const ace = overlayRow(overlay, ACE);
    const bee = overlayRow(overlay, BEE);
    await expect(ace).toBeVisible();
    await expect(bee).toBeVisible();
    // Lineup order: Ace is on Seat 1.
    const rows = overlay.getByRole('table', { name: 'Pilots in this heat' }).locator('tbody tr');
    await expect(rows).toHaveCount(2);
    await expect(rows.nth(0)).toContainText(ACE);
    await expect(rows.nth(1)).toContainText(BEE);
    await expect(stat(ace, 'laps')).toHaveText('0');

    // ── Racing: the clock counts up and laps follow the crossings ────────────────────────
    const stop = page.getByRole('button', { name: 'Stop', exact: true });
    await expect(stop).toBeVisible({ timeout: 20_000 });
    await expect(overlay.getByTestId('overlay-state')).toHaveText('Live', { timeout: 15_000 });
    await expect(overlay.getByRole('timer')).toHaveAttribute('data-mode', 'elapsed');

    const aceCrossing = page.getByRole('button', { name: 'Crossing for Seat 1' });
    const beeCrossing = page.getByRole('button', { name: 'Crossing for Seat 2' });
    // The first crossing on a seat starts lap 1; each one after ends a lap.
    await aceCrossing.click();
    await beeCrossing.click();
    await expect(page.getByText('On course')).toHaveCount(2);
    for (const laps of [1, 2]) {
      await aceCrossing.click();
      await expect(stat(ace, 'laps')).toHaveText(String(laps));
      await beeCrossing.click();
      await expect(stat(bee, 'laps')).toHaveText(String(laps));
    }
    await expect(stat(ace, 'place')).toHaveText('1');
    await expect(stat(bee, 'place')).toHaveText('2');
    await expect(stat(ace, 'last')).toHaveText(/^\d+\.\d{3}$/);
    await expect(stat(ace, 'best')).toHaveText(/^\d+\.\d{3}$/);

    // A picture of it mid-race, over a stand-in for dark video.
    mkdirSync(ARTIFACTS, { recursive: true });
    await overlay.addStyleTag({
      content:
        'html { background: linear-gradient(135deg, #1f2a3a, #0b0f16 60%, #233047) !important; }'
    });
    await overlay.screenshot({ path: resolve(ARTIFACTS, 'overlay-shot.png') });

    await aceCrossing.click();
    await expect(stat(ace, 'laps')).toHaveText('3');
    await beeCrossing.click();
    await expect(stat(bee, 'laps')).toHaveText('3');

    // ── No wire handle is on the overlay ─────────────────────────────────────────────────
    const state = (await (await page.request.get(`/events/${eventId}/state`)).json()) as {
      phases: { def: { id: string } }[];
      races: { race: string }[];
    };
    const raw = [
      eventId,
      state.phases[0].def.id,
      state.races[0].race,
      '#1',
      'bench',
      'seat-1',
      'seat-2',
      ...made.pilots
    ];
    const racingText = await overlay.locator('body').innerText();
    for (const handle of raw) expect(racingText).not.toContain(handle);

    // ── Stop and save: the overlay shows the stopped race, then the saved result ─────────
    await stop.click();
    await expect(overlay.getByTestId('overlay-state')).toHaveText('Finished', { timeout: 15_000 });
    await page.getByRole('button', { name: 'Save', exact: true }).click();
    await expect(overlay.getByTestId('overlay-state')).toHaveText('Result', { timeout: 15_000 });
    await expect(overlay.getByTestId('overlay-heat')).toHaveText(HEAT);
    await expect(stat(ace, 'laps')).toHaveText('3');
    const resultText = await overlay.locator('body').innerText();
    for (const handle of raw) expect(resultText).not.toContain(handle);

    // ── The Director moves to another event, and the overlay follows without a reload ────
    const secondId = await createEvent(page, SECOND_EVENT);
    made.events.push(secondId);
    expect((await page.request.put('/active-event', { data: { id: secondId } })).ok()).toBe(true);
    await expect(overlay.getByTestId('overlay-idle')).toHaveText(SECOND_EVENT, {
      timeout: 20_000
    });
    await expect(overlay.getByText(HEAT)).toHaveCount(0);
    await expect(overlay.getByText(ACE)).toHaveCount(0);
    const idleText = await overlay.locator('body').innerText();
    for (const handle of [...raw, secondId]) expect(idleText).not.toContain(handle);

    // ── A name the console has no overlay for draws nothing, not the hub ─────────────────
    // Once by editing the hash of the open page, once as a fresh load.
    await overlay.goto('/#/overlay/no-such-overlay');
    await expect(overlay.locator('body')).toHaveText('', { timeout: 15_000 });
    await overlay.reload();
    await expect(overlay.locator('.gridfpv-root')).toBeAttached({ timeout: 15_000 });
    await expect(overlay.locator('body')).toHaveText('');
    await expect(overlay.getByRole('button')).toHaveCount(0);

    expect(overlayErrors).toEqual([]);
  } finally {
    await overlay.close();
    for (const id of made.events) await page.request.delete(`/events/${id}`);
    for (const id of made.pilots) await page.request.delete(`/pilots/${id}`);
    if (before.event) {
      await page.request.put('/active-event', { data: { id: before.event.id } });
    }
  }
});

test('the Overlays page lists the overlay with its URL and a live preview', async ({
  page,
  baseURL
}) => {
  const before = (await (await page.request.get('/active-event')).json()) as {
    event: { id: string } | null;
  };
  const name = `Overlay Card ${STAMP}`;
  const eventId = await createEvent(page, name);
  try {
    expect((await page.request.put('/active-event', { data: { id: eventId } })).ok()).toBe(true);
    await page.goto('/#/event/overlays');
    await expect(page.getByRole('heading', { name: 'Overlays', level: 1 })).toBeVisible({
      timeout: 15_000
    });
    await expect(page.locator('header a.event')).toContainText(name);

    const card = page.getByRole('region', { name: 'Race overlay' });
    const origin = new URL(baseURL ?? '').origin;
    const url = card.getByLabel('Race overlay URL');
    await expect(url).toHaveText(`${origin}/#/overlay/race`);
    await expect(card.getByRole('link', { name: 'Open in a new tab' })).toHaveAttribute(
      'href',
      `${origin}/#/overlay/race`
    );
    await card.getByRole('radio', { name: 'Dark text, for bright video' }).click();
    await expect(url).toHaveText(`${origin}/#/overlay/race/light`);
    await expect(card.getByRole('button', { name: 'Copy URL' })).toBeVisible();

    // The preview is the real overlay page in a frame, following the same event.
    await card.getByRole('button', { name: 'Show preview' }).click();
    const frame = page.frameLocator('iframe[title="Race overlay preview"]');
    await expect(frame.getByRole('region', { name: 'Race overlay' })).toHaveAttribute(
      'data-theme',
      'light',
      { timeout: 15_000 }
    );
    await expect(frame.getByTestId('overlay-idle')).toHaveText(name, { timeout: 15_000 });
    // Scaled to fit the card, not drawn at 1920 wide.
    const box = await page.locator('iframe[title="Race overlay preview"]').boundingBox();
    const cardBox = await card.boundingBox();
    expect(box && cardBox && box.width <= cardBox.width + 1).toBe(true);
  } finally {
    await page.request.delete(`/events/${eventId}`);
    if (before.event) {
      await page.request.put('/active-event', { data: { id: before.event.id } });
    }
  }
});
