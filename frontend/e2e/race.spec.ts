/**
 * Full RD console click-through against a real Director (#13, v0.4 Director wiring +
 * observability harness; entry reshaped for the home-hub IA, #118).
 *
 * This is the deliverable proof: a person opens the RD console, lands on the **home hub**, opens
 * the **Events** page, enters **Practice**, defines a heat with named pilots, runs it, **watches
 * the live laps climb in the rendered DOM**, finishes + scores, and reads results with the pilots
 * and their lap counts — every step a real click/input in headless chromium, every command on the
 * real control path, every lap from the real built-in sim source. Nothing is mocked.
 *
 * The Director is booted with **no token configured**, so control is **open** (full-trust by
 * default, #72 Slice 1b): the run goes hub → Events → Practice → build heat → control with **no
 * token step** — the lazy prompt never fires. (The token-gated path is a separate, optional concern.)
 *
 * The lap-counts-climbing assertion is the load-bearing one: it polls the per-pilot lap
 * numbers rendered in the HeatSheet and asserts they increase over a couple of seconds —
 * proving the live read stream (protocol-client) and the reactive `Session` push updates
 * all the way through the real render path.
 *
 * The Director (built console SPA + known RD token + fast sim) is a worker-scoped fixture in
 * `./observability.ts` (boot harness: `../test-harness/director.ts`); `baseURL` points at it.
 * Importing `test`/`expect` from `./observability.js` (not `@playwright/test`) means this spec
 * now fails LOUD: if it ever breaks, the failure output carries the full-stack dump — browser
 * console, page errors, WS frames, and the Director's server log — together.
 */
import { expect, test } from './observability.js';

const PILOTS = ['Ace', 'Bee', 'Cee'];
const HEAT_ID = 'q-1';

test('RD drives a full basic sim race through the console UI', async ({ page }) => {
  // ── Open the console: the home hub is the landing screen (#118) ──────────────────────
  await page.goto('/');

  // ── Hub → Events page → enter Practice (no token needed to browse/enter) ─────────────
  // The hub's three cards land on Pilots/Events/Timers pages; the Events page is the former
  // picker, which renders Practice prominently. The Director is the page's own origin.
  await page.getByRole('button', { name: /Events/ }).click();
  await expect(page.getByRole('heading', { name: 'Choose an event' })).toBeVisible({
    timeout: 15_000
  });
  await page
    .getByRole('button', { name: /Practice/ })
    .first()
    .click();

  // The shell is up: the Live control screen is the default landing screen.
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible();
  // The live read stream connects against the Director and settles on `live` (it passes
  // through connecting → snapshotting on the way).
  await expect(page.locator('.conn-label')).toHaveText('live', { timeout: 15_000 });

  // ── #90: the active event is Director state — a reload RESUMES into it, not the picker ──
  // Entering Practice persisted it as the Director's active event (`PUT /active-event`), so a
  // full page reload reads `GET /active-event` and re-enters the workspace directly — the
  // picker's "Choose an event" heading must NOT reappear.
  await page.reload();
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('heading', { name: 'Choose an event' })).toBeHidden();
  await expect(page.locator('.conn-label')).toHaveText('live', { timeout: 15_000 });

  // ── Define a heat with named pilots ──────────────────────────────────────────────────
  const newHeat = page.getByRole('region', { name: 'New heat' });
  await newHeat.getByLabel('Heat id').fill(HEAT_ID);
  // Two default pilot rows + one added → three named pilots.
  await newHeat.getByRole('button', { name: 'Add pilot' }).click();
  for (let i = 0; i < PILOTS.length; i++) {
    await newHeat.getByLabel(`Pilot ${i + 1} name`).fill(PILOTS[i]);
  }

  // ── Schedule it: the Director is open (no token configured), so this control action goes
  //    straight through — NO token prompt appears (full-trust by default, #72) ──────────────
  await newHeat.getByRole('button', { name: 'Schedule heat' }).click();
  // The lazy token prompt must NOT appear against an open Director.
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();

  // The heat lands on the timer: current heat + the lineup show, phase Scheduled.
  await expect(page.locator('.heat-id .value')).toHaveText(HEAT_ID);
  const heatSheet = page.getByRole('region', { name: 'Heat sheet' });
  for (const pilot of PILOTS) {
    await expect(heatSheet.getByText(pilot, { exact: true })).toBeVisible();
  }
  await expect(page.locator('.phase').first()).toHaveText('Scheduled');

  // ── Run the heat loop: Stage → Arm → Start ───────────────────────────────────────────
  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Staged');

  await page.getByRole('button', { name: 'Arm', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Armed');

  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Running');

  // ── Watch the live laps climb in the DOM: poll the rendered per-pilot lap counts ──────
  // THIS is the proof the live-stream + reactive-Session path renders updates: the HeatSheet
  // `.laps` cells are filled by the live read stream, and their sum must increase over time.
  const lapCells = heatSheet.locator('.laps');
  const totalLaps = async () => {
    const texts = await lapCells.allTextContents();
    return texts.reduce((sum, t) => sum + (parseInt(t.match(/(\d+)/)?.[1] ?? '0', 10) || 0), 0);
  };
  // First, some laps appear in the render.
  await expect
    .poll(totalLaps, { timeout: 30_000, message: 'live laps should appear in the DOM' })
    .toBeGreaterThan(0);
  const early = await totalLaps();
  // Then the count climbs as the sim keeps emitting — the live render path is live.
  await expect
    .poll(totalLaps, { timeout: 30_000, message: 'live lap counts should climb in the DOM' })
    .toBeGreaterThan(early);

  // ── Finish the window ────────────────────────────────────────────────────────────────
  await page.getByRole('button', { name: 'Finish', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Finished');

  // ── Score the heat ───────────────────────────────────────────────────────────────────
  await page.getByRole('button', { name: 'Score', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Scored');

  // ── Read the results: the Results screen shows the pilots, lap counts, and an order ───
  await page.getByRole('button', { name: /Results/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  const leaderboard = results.getByRole('table', { name: 'Heat leaderboard' });
  await expect(leaderboard).toBeVisible();

  // Every pilot appears in the result with a decided finishing order (positions 1..n).
  const rows = leaderboard.locator('tbody tr');
  await expect(rows).toHaveCount(PILOTS.length);
  for (const pilot of PILOTS) {
    await expect(leaderboard.getByRole('cell', { name: pilot, exact: true })).toBeVisible();
  }
  // The leader (position 1) banked at least one lap — a real, scored result.
  const leaderLaps = await rows.first().locator('.laps').textContent();
  expect(parseInt(leaderLaps ?? '0', 10)).toBeGreaterThan(0);
  // Positions are decided and start at 1.
  const firstPos = await rows.first().locator('.pos .badge').textContent();
  expect(firstPos?.trim()).toBe('1');
});

/**
 * The home-hub navigation itself (#118): from the hub, each of the three cards opens its page and
 * the breadcrumb's "Home" crumb returns to the hub; the Timers page renders the registry manager.
 * No event is entered here — this exercises the app-level shell, not the workspace.
 */
test('home hub navigates to each page and back, with working breadcrumbs', async ({ page }) => {
  await page.goto('/');

  // The worker's Director is shared, so a prior spec may have left an active event — on load the
  // shell would then resume into that workspace (#90). Wait for the shell to settle (either the
  // hub's Pilots card or the workspace's Live-control nav appears), and if we resumed into the
  // workspace use the brand/logo (a Home root, #118) to return to the hub.
  const pilotsCard = page.getByRole('heading', { name: 'Pilots' });
  const liveNav = page.getByRole('button', { name: /Live control/ });
  await expect(pilotsCard.or(liveNav).first()).toBeVisible({ timeout: 15_000 });
  if (await liveNav.isVisible().catch(() => false)) {
    // Resumed into the workspace — the breadcrumb's Home crumb returns to the hub.
    await page
      .getByRole('navigation', { name: 'Breadcrumb' })
      .getByRole('button', { name: 'Home' })
      .click();
  }

  // The hub: three cards.
  await expect(pilotsCard).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('heading', { name: 'Events' })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Timers' })).toBeVisible();

  // Home → Timers: the page renders the shared registry manager (the built-in Mock is listed),
  // and the breadcrumb shows Home › Timers.
  await page.getByRole('button', { name: /Timers/ }).click();
  await expect(page.getByRole('heading', { name: 'Timers', level: 1 })).toBeVisible();
  await expect(page.getByRole('list', { name: 'Configured timers' })).toBeVisible({
    timeout: 15_000
  });
  const crumbs = page.getByRole('navigation', { name: 'Breadcrumb' });
  await expect(crumbs.getByText('Timers')).toBeVisible();
  // Breadcrumb Home returns to the hub.
  await crumbs.getByRole('button', { name: 'Home' }).click();
  await expect(page.getByRole('heading', { name: 'Events' })).toBeVisible();

  // Home → Pilots: the placeholder page with the "registration UI coming" note.
  await page.getByRole('button', { name: /Pilots/ }).click();
  await expect(page.getByRole('heading', { name: 'Pilots', level: 1 })).toBeVisible();
  await expect(page.getByText(/registration UI/i)).toBeVisible({ timeout: 15_000 });
  await page
    .getByRole('navigation', { name: 'Breadcrumb' })
    .getByRole('button', { name: 'Home' })
    .click();
  await expect(page.getByRole('heading', { name: 'Timers' })).toBeVisible();

  // Home → Events: the picker (former landing), reachable as a page now.
  await page.getByRole('button', { name: /Events/ }).click();
  await expect(page.getByRole('heading', { name: 'Choose an event' })).toBeVisible({
    timeout: 15_000
  });
  await page
    .getByRole('navigation', { name: 'Breadcrumb' })
    .getByRole('button', { name: 'Home' })
    .click();
  await expect(page.getByRole('heading', { name: 'Pilots' })).toBeVisible();
});
