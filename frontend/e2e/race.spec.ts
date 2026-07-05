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

test('RD drives a full basic sim race through the console UI', async ({ page, director }) => {
  // ── Open the console: the home hub is the landing screen (#118) ──────────────────────
  await page.goto('/');

  // ── Hub → Events page → enter Practice (no token needed to browse/enter) ─────────────
  // The hub's cards land on Pilots/Classes/Events/Timers pages; the Events page is the former
  // picker, which renders Practice prominently. The Director is the page's own origin. The
  // worker's Director may already have an active event from a prior spec (#90): on a fresh load
  // the hash is authoritative (#118) so we land on the hub even then, and clicking Events either
  // shows the picker (nothing active) or auto-enters the active event's workspace — handle both.
  const liveNav = page.getByRole('button', { name: /Live control/ });
  await page.getByRole('button', { name: /Events/ }).click();
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

  // The shell is up: the Live control screen is the default landing screen.
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible();
  // The live read stream connects against the Director and settles on `live` (it passes
  // through connecting → snapshotting on the way).
  await expect(page.locator('.conn-label')).toHaveText('live', { timeout: 15_000 });

  // ── Schedule a heat with named pilots over the real control path ──────────────────────
  // The free-text NewHeat form is retired (race redesign Slice 3b); heats are built from real
  // round/class members in the Rounds & Heats stage (covered by `rounds.spec.ts`). This race
  // spec's focus is *running* a heat, so it schedules one straight over the open control path
  // (the Director is booted with no token configured) — exactly what the Heats UI emits.
  const ack = await page.request.post(`${director.baseUrl}/events/practice/control`, {
    headers: { 'Content-Type': 'application/json' },
    data: { ScheduleHeat: { heat: HEAT_ID, lineup: PILOTS } }
  });
  expect(ack.ok()).toBeTruthy();
  // Explicitly focus THIS heat as the current heat (SetCurrentHeat) over the control path. The
  // shared worker Director's `current_heat` stays pinned to whatever heat a prior spec last touched
  // (it never resets between specs), and filling a heat does NOT steal Live focus (current-heat.spec.ts)
  // — so this server-side selection keeps the run independent of spec order.
  const focused = await page.request.post(`${director.baseUrl}/events/practice/control`, {
    headers: { 'Content-Type': 'application/json' },
    data: { SetCurrentHeat: { heat: HEAT_ID } }
  });
  expect(focused.ok()).toBeTruthy();

  // ── #90: the active event is Director state — a reload RESUMES into it, not the picker ──
  // Entering Practice persisted it as the Director's active event (`PUT /active-event`), so a
  // full page reload reads `GET /active-event` and re-enters the workspace directly — the
  // picker's "Choose an event" heading must NOT reappear.
  await page.reload();
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('heading', { name: 'Choose an event' })).toBeHidden();
  await expect(page.locator('.conn-label')).toHaveText('live', { timeout: 15_000 });

  // The heat lands on the timer: current heat + the lineup show, phase Scheduled.
  await expect(page.locator('.heat-id .value')).toHaveText(HEAT_ID);
  const heatSheet = page.getByRole('region', { name: 'Heat sheet' });
  for (const pilot of PILOTS) {
    await expect(heatSheet.getByText(pilot, { exact: true })).toBeVisible();
  }
  await expect(page.locator('.phase').first()).toHaveText('Scheduled');

  // ── Run the heat loop: Stage → Start, then the runtime auto-advances ───────────────────
  // Heat-lifecycle Slices 1–3: `Stage` calls the field to the line; in Staged the console shows the
  // informational **staging countdown** (no auto-advance). `Start` *arms* the heat (Staged → Armed)
  // and runs the start procedure; the Director's runtime clock then auto-advances Armed → Running
  // after a randomized hold — there is NO manual "Arm"/"Start→Running" button anymore.
  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Staged');
  // The staging countdown is shown (informational only).
  await expect(page.getByRole('status', { name: 'Staging countdown' })).toBeVisible();

  await page.getByRole('button', { name: 'Start', exact: true }).click();
  // Armed: the console shows the generic "arming… stand by" state (the random hold is hidden — no
  // precise countdown), then the runtime auto-advances to Running and the race clock takes over.
  await expect(page.getByRole('status', { name: 'Arming' })).toBeVisible();
  await expect(page.locator('.phase').first()).toHaveText('Running', { timeout: 15_000 });

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

  // ── Mid-race: the header clock and the HUD clock AGREE (the #62 follow-up fix) ──────────
  // Both are independent `useRaceClock` instances, now anchored to the server's `race_started_at`
  // rather than to whenever each mounted. Navigating AWAY from Live and BACK remounts the HUD clock
  // mid-race (the exact late-join the bug mis-handled — it used to count from arrival and read ~7s
  // behind the always-mounted header). After the remount, the two must read the same elapsed.
  const parseClock = (t: string | null) => {
    const m = /(\d+):(\d+)\.(\d+)/.exec(t ?? '');
    return m ? (+m[1] * 60 + +m[2]) * 1000 + +m[3] : NaN;
  };
  const hudClockLive = page.locator('.hud-clock .gridfpv-race-clock');
  const headerClockLive = page.locator('.ctx-clock .gridfpv-race-clock');
  await expect(hudClockLive).toBeVisible();
  await expect(headerClockLive).toBeVisible();
  // Leave Live (remounting nothing in the header — it is persistent), then return so the HUD clock
  // mounts fresh well after race-go.
  await page.getByRole('button', { name: /Results/ }).click();
  await page.waitForTimeout(1200);
  await page.getByRole('button', { name: /Live control/ }).click();
  await expect(hudClockLive).toBeVisible();
  // Sample both in the same tick: the freshly-mounted HUD must agree with the long-lived header
  // (within one 50ms tick of sampling jitter), NOT lag by the time we were away. This is the bug.
  const hudMid = parseClock(await hudClockLive.textContent());
  const headerMid = parseClock(await headerClockLive.textContent());
  expect(Math.abs(hudMid - headerMid)).toBeLessThan(150);
  // And it must read the REAL elapsed (well past 0), proving it counted from race-go not arrival.
  expect(hudMid).toBeGreaterThan(1000);

  // ── End the window: ForceEnd is the runtime-clock override (the old manual "Finish") ──
  await page.getByRole('button', { name: 'ForceEnd', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Unofficial');

  // ── The race clock must FREEZE at Unofficial (the rename-regression fix) ───────────────
  // The race has ended (only the result isn't finalized yet), so the heat clock stops at the
  // race-end instant instead of free-running. Read the HUD clock right after the transition, wait,
  // and assert it has NOT advanced. The persistent header clock (#85) mirrors the same source.
  const hudClock = page.locator('.hud-clock .gridfpv-race-clock');
  const headerClock = page.locator('.ctx-clock .gridfpv-race-clock');
  await expect(hudClock).toBeVisible();
  // The header clock stays VISIBLE on end too — it no longer vanishes the instant the race closes
  // (the bug fix #2). Both are independent `useRaceClock` instances, so each must FREEZE on its own.
  await expect(headerClock).toBeVisible();
  const frozenHud = await hudClock.textContent();
  const frozenHeader = await headerClock.textContent();
  // Both freeze at the EXACT server-side duration (`race_ended_at - race_started_at`), so they read
  // the IDENTICAL value — not merely "within a few ms". This is the #62 follow-up: no per-client
  // drift and no poll-late overshoot (the frozen reading is the precise server race length).
  expect(frozenHud).toBe(frozenHeader);
  await page.waitForTimeout(1500);
  // The bug: the clock free-ran. The fix: each clock STOPS at its race-end value and does not tick.
  expect(await hudClock.textContent()).toBe(frozenHud);
  expect(await headerClock.textContent()).toBe(frozenHeader);

  // Screenshot the frozen clock in Unofficial for the PR.
  const shots = process.env.GRIDFPV_SHOTS;
  if (shots) {
    await page.screenshot({ path: `${shots}/race-clock-frozen-unofficial.png`, fullPage: true });
  }

  // ── Finalize the heat ────────────────────────────────────────────────────────────────
  await page.getByRole('button', { name: 'Finalize', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Final');

  // The clock stays frozen through Final (finalize does not restart it).
  await page.waitForTimeout(1000);
  expect(await hudClock.textContent()).toBe(frozenHud);

  // ── Revert re-opens the finalized heat, then Finalize locks it back in ────────────────
  await page.getByRole('button', { name: 'Revert', exact: true }).click();
  await page.getByRole('button', { name: 'Confirm' }).click();
  await expect(page.locator('.phase').first()).toHaveText('Unofficial');
  await page.getByRole('button', { name: 'Finalize', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Final');

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
 * Friendly names everywhere in Live control — the human-readable-identifiers fix.
 *
 * The bug: Live control rendered raw ids/refs. This proves the fix end-to-end against a real
 * Director: a heat whose seats are bound (via `Register`) to **directory pilots** renders those
 * pilots' **callsigns** in the lineup/heat-sheet — not the bare competitor refs — and an **on-deck**
 * heat shows its name. The pilots directory + the registration both go over the real REST/control
 * paths; the browser's own live stream drives the render (nothing mocked).
 */
test('Live control shows pilot callsigns (not refs) and an on-deck heat', async ({
  page,
  director
}) => {
  await page.goto('/');

  // Enter Practice (handle the active-event-resume / picker branches like the main race test).
  const liveNav = page.getByRole('button', { name: /Live control/ });
  await page.getByRole('button', { name: /Events/ }).click();
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
  await expect(page.locator('.conn-label')).toHaveText('live', { timeout: 15_000 });

  const control = (cmd: unknown) =>
    page.request.post(`${director.baseUrl}/events/practice/control`, {
      headers: { 'Content-Type': 'application/json' },
      data: cmd
    });

  // Two directory pilots (real callsigns) created over the open `POST /pilots` path.
  const mkPilot = async (callsign: string) =>
    (
      await (
        await page.request.post(`${director.baseUrl}/pilots`, {
          headers: { 'Content-Type': 'application/json' },
          data: { callsign }
        })
      ).json()
    ).id as string;
  const maverick = await mkPilot(`Maverick-${Date.now()}`);
  const goose = await mkPilot(`Goose-${Date.now()}`);

  // A heat whose two seats are the bare refs `fn-a`/`fn-b`; bind each to a directory pilot via the
  // real `Register` command, then focus this heat. The lineup must then render the callsigns.
  const HEAT = `fn-${Date.now()}`;
  const ONDECK = `fn-deck-${Date.now()}`;
  expect(
    (await control({ ScheduleHeat: { heat: HEAT, lineup: ['fn-a', 'fn-b'] } })).ok()
  ).toBeTruthy();
  expect(
    (await control({ Register: { adapter: '', competitor: 'fn-a', pilot: maverick } })).ok()
  ).toBeTruthy();
  expect(
    (await control({ Register: { adapter: '', competitor: 'fn-b', pilot: goose } })).ok()
  ).toBeTruthy();
  expect((await control({ SetCurrentHeat: { heat: HEAT } })).ok()).toBeTruthy();
  // A second heat appended keeps it in the schedule as a candidate on-deck (fill-no-steal keeps
  // focus on HEAT). Which heat is *on deck* depends on the shared Director's leftover schedule, so
  // the assertions below don't pin it to ONDECK — only that the on-deck renders a heat name.
  expect((await control({ ScheduleHeat: { heat: ONDECK, lineup: ['fn-c'] } })).ok()).toBeTruthy();

  // The current-heat title is friendly-named (an untagged heat falls back to its bare id, here HEAT).
  await expect(page.locator('.heat-id .value')).toHaveText(HEAT, { timeout: 15_000 });

  // The heat-sheet lineup now reads the CALLSIGNS — never the raw `fn-a`/`fn-b` refs.
  const heatSheet = page.getByRole('region', { name: 'Heat sheet' });
  await expect(heatSheet.getByText(/^Maverick-/)).toBeVisible({ timeout: 15_000 });
  await expect(heatSheet.getByText(/^Goose-/)).toBeVisible();
  await expect(heatSheet.getByText('fn-a', { exact: true })).toHaveCount(0);
  await expect(heatSheet.getByText('fn-b', { exact: true })).toHaveCount(0);

  // The HeatSheet heading is the friendly heat name (here the id fallback for an untagged heat) —
  // resolved by the screen, not the raw `current_heat` straight off the stream.
  await expect(heatSheet.getByRole('heading')).toHaveText(HEAT);

  // The on-deck slot renders a heat (a friendly name / id) and never an empty value or a raw seat
  // ref — proving on-deck goes through the same resolver. (The exact heat depends on the shared
  // Director's schedule, so we assert it's a non-empty heat label, not a specific id.)
  const onDeck = page.locator('.hud-ondeck .value');
  if (await onDeck.count()) {
    await expect(onDeck).not.toHaveText('');
    await expect(onDeck).not.toHaveText(/^node-\d+$/);
  }
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

  // Home → Pilots: the directory page hosting the shared PilotManager (#74) — its "+ Add pilot"
  // action and the directory list are present.
  await page.getByRole('button', { name: /Pilots/ }).click();
  await expect(page.getByRole('heading', { name: 'Pilots', level: 1 })).toBeVisible();
  await expect(page.getByRole('button', { name: '+ Add pilot' })).toBeVisible({ timeout: 15_000 });
  await page
    .getByRole('navigation', { name: 'Breadcrumb' })
    .getByRole('button', { name: 'Home' })
    .click();
  await expect(page.getByRole('heading', { name: 'Timers' })).toBeVisible();

  // Home → Events: the picker (former landing), reachable as a page now. The worker's Director may
  // still have an active event from an earlier spec, in which case `currentEvent` is hydrated and
  // the Events page auto-enters that event's workspace (#118); "Switch event" then returns to the
  // picker. So we land on the picker whether or not an event was active.
  await page.getByRole('button', { name: /Events/ }).click();
  const picker = page.getByRole('heading', { name: 'Choose an event' });
  await expect(picker.or(liveNav).first()).toBeVisible({ timeout: 15_000 });
  if (await liveNav.isVisible().catch(() => false)) {
    await page.getByRole('button', { name: /Switch event/ }).click();
  }
  await expect(picker).toBeVisible({ timeout: 15_000 });
  await page
    .getByRole('navigation', { name: 'Breadcrumb' })
    .getByRole('button', { name: 'Home' })
    .click();
  await expect(page.getByRole('heading', { name: 'Pilots' })).toBeVisible();
});
