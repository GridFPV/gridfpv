/**
 * The race overlay, end to end, in real headless chromium against the worker's real Director: an
 * ordinary event's heat is scheduled and started over the control path, then the OBS browser-source
 * page (`#/overlay/race`) renders it — the event and heat names, the clock, a row per pilot with
 * climbing laps — read-only, on a transparent document, in both contrasts. Then the workspace's
 * **Overlays** tab offers that page's URL and frames a live preview of it.
 *
 * Nothing about the event is special: the overlay follows the Director's active event, whatever it
 * is. Importing `test`/`expect` from `./observability.js` means a failure carries the full-stack
 * dump (browser console, page errors, the Director's log).
 */
import { expect, test } from './observability.js';

const PILOTS = ['Ace', 'Bee', 'Cee'];
const HEAT_ID = 'ov-1';
const ROUND_LABEL = 'E2E-Overlay';

test('the race overlay follows an ordinary event’s heat, and the Overlays tab hands out its URL', async ({
  page,
  director
}) => {
  // ── Make the event active, the way an RD does: open it from the Events page ────────────────
  await page.goto('/');
  await page.getByRole('button', { name: /Events/ }).click();
  const liveNav = page.getByRole('button', { name: /Race control/ });
  await expect(
    page.getByRole('heading', { name: 'Choose an event' }).or(liveNav).first()
  ).toBeVisible({ timeout: 15_000 });
  if (
    await page
      .getByRole('heading', { name: 'Choose an event' })
      .isVisible()
      .catch(() => false)
  ) {
    await page
      .getByRole('button', { name: /Practice/ })
      .first()
      .click();
  }
  await expect(liveNav).toBeVisible();

  // ── A round and a heat of three, focused and started over the open control path ────────────
  const roundRes = await page.request.post(`${director.eventRoot}/rounds`, {
    headers: { 'Content-Type': 'application/json' },
    data: {
      label: ROUND_LABEL,
      classes: [],
      format: 'timed_qual',
      params: { rounds: '1' },
      win_condition: 'BestLap',
      time_limit_secs: 60,
      seeding: 'FromRoster',
      channel_mode: 'PerHeat'
    }
  });
  expect(roundRes.ok()).toBeTruthy();
  const roundId = ((await roundRes.json()) as { id: string }).id;
  for (const command of [
    { ScheduleHeat: { heat: HEAT_ID, lineup: PILOTS, round: roundId } },
    { SetCurrentHeat: { heat: HEAT_ID } },
    { Stage: { heat: HEAT_ID } },
    { Start: { heat: HEAT_ID } }
  ]) {
    const ack = await page.request.post(`${director.eventRoot}/control`, {
      headers: { 'Content-Type': 'application/json' },
      data: command
    });
    expect(ack.ok(), JSON.stringify(command)).toBeTruthy();
  }

  // ── The overlay: the OBS browser source follows the active event's heat, read-only ─────────
  await page.goto(`/#/overlay/race`);
  const overlay = page.getByLabel('Race overlay');
  await expect(overlay).toBeVisible({ timeout: 15_000 });
  // The event and the heat, by their friendly names — never a raw id.
  await expect(page.getByText(`Practice · ${ROUND_LABEL} Heat 1`)).toBeVisible({ timeout: 15_000 });
  // A 60 s race time: the primary clock counts down, as the console's does.
  await expect(page.getByRole('timer', { name: /Time remaining/ })).toBeVisible({
    timeout: 15_000
  });
  await expect(page.getByText('LIVE')).toBeVisible({ timeout: 15_000 });
  // One row per pilot, named; their laps climb as the sim flies.
  const table = page.getByRole('table', { name: 'Pilots in this heat' });
  for (const pilot of PILOTS) {
    await expect(table.getByText(pilot, { exact: true })).toBeVisible();
  }
  const lapCells = table.locator('tbody tr td:nth-child(3)');
  const totalLaps = async () => {
    const texts = await lapCells.allTextContents();
    return texts.reduce((sum, t) => sum + (parseInt(t.trim(), 10) || 0), 0);
  };
  await expect
    .poll(totalLaps, { timeout: 30_000, message: 'laps should appear on the overlay' })
    .toBeGreaterThan(0);
  // The document is flagged transparent for the keyer.
  await expect(page.locator('html')).toHaveAttribute('data-overlay', 'race');
  await expect(page.locator('body')).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');

  // The light variant: same overlay, dark text on a light panel, still transparent underneath.
  await page.goto(`/#/overlay/race/light`);
  await expect(page.getByLabel('Race overlay')).toHaveAttribute('data-theme', 'light');
  await expect(page.getByText(`Practice · ${ROUND_LABEL} Heat 1`)).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('body')).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');

  // ── The Overlays tab inside the event: the URL for OBS, and a live preview of the page ─────
  await page.goto(`/#/event/overlays`);
  await expect(page.getByRole('heading', { name: 'Overlays' })).toBeVisible({ timeout: 15_000 });
  const url = page.getByLabel('Race timer URL');
  await expect(url).toHaveText(new RegExp(`^${director.baseUrl}/#/overlay/race$`));
  await page.getByRole('radio', { name: 'Dark text on light' }).click();
  await expect(url).toHaveText(/#\/overlay\/race\/light$/);
  // The preview frames the real route; inside it, the same heat is rendering.
  const preview = page.frameLocator('iframe[title="Race timer preview"]');
  await expect(preview.getByLabel('Race overlay')).toBeVisible({ timeout: 15_000 });
  await expect(preview.getByText(`Practice · ${ROUND_LABEL} Heat 1`)).toBeVisible({
    timeout: 15_000
  });

  // ── Tidy: end the heat so the worker's Director is quiet for the next spec ─────────────────
  await page.request.post(`${director.eventRoot}/control`, {
    headers: { 'Content-Type': 'application/json' },
    data: { ForceEnd: { heat: HEAT_ID } }
  });
});
