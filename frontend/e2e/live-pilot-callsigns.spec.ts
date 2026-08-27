/**
 * Pilot callsigns in Live control resolve from the **roster binding**, BEFORE the heat runs — the
 * regression #212's e2e missed.
 *
 * #212 resolved a competitor ref → callsign only from the live `progress[].pilot` field. But a
 * real `FromRoster` heat seeds each competitor ref to the **pilot id itself** and emits NO
 * `CompetitorRegistered` event, so `progress.pilot` is `null` in every phase (Scheduled → Running)
 * — confirmed on a throwaway Director. So Live control fell back to the raw pilot id everywhere a
 * pilot appears (heat sheet, leaderboard, channels panel) for a heat that is merely scheduled.
 *
 * This spec sets up a real roster-seeded heat over the write paths, FILLS it (it lands **Scheduled
 * — not run**), focuses it as the current heat, opens Live control, and asserts the lineup shows
 * the pilot **callsigns** (never the raw pilot ids) — a NOT-running heat. Then it stages it (still
 * not racing) and re-asserts. Nothing is mocked; every callsign comes from the real `/pilots`
 * directory joined to the real scheduled lineup.
 *
 * Importing `test`/`expect` from `./observability.js` means a failure carries the full-stack dump
 * (browser console, page errors, WS frames, the Director's server log).
 */
import { expect, test } from './observability.js';

/** Get the shared worker Director into the Practice event's workspace. */
async function enterPractice(page: import('@playwright/test').Page) {
  const liveNav = page.getByRole('button', { name: /Race control/ });
  const eventsCard = page.getByRole('button', { name: /Events/ });
  await expect(liveNav.or(eventsCard).first()).toBeVisible({ timeout: 15_000 });
  if (!(await liveNav.isVisible().catch(() => false))) {
    await eventsCard.click();
    const picker = page.getByRole('heading', { name: 'Choose an event' });
    await expect(picker.or(liveNav).first()).toBeVisible({ timeout: 15_000 });
    if (await picker.isVisible().catch(() => false)) {
      await page
        .getByRole('button', { name: /Practice/ })
        .first()
        .click();
    }
    await expect(liveNav).toBeVisible({ timeout: 15_000 });
  }
}

test('Live control shows pilot callsigns for a NOT-running roster-seeded heat (not raw ids)', async ({
  page,
  director
}) => {
  const base = director.baseUrl;
  const ev = director.eventRoot;
  const json = { headers: { 'Content-Type': 'application/json' } };
  const SUFFIX = Date.now();
  const ACE = `E2E-LiveCall-Ace-${SUFFIX}`;
  const BEE = `E2E-LiveCall-Bee-${SUFFIX}`;
  const ROUND_LABEL = `E2E-LiveCallRound-${SUFFIX}`;

  // ── Set up over the real write paths: Open Class selected, two rostered members, a round ──────
  const classes = (await (await page.request.get(`${base}/classes`)).json()) as Array<{
    id: string;
    name: string;
  }>;
  const classId = classes.find((c) => c.name === 'Open Class')!.id;

  const mkPilot = async (callsign: string) => {
    const p = (await (
      await page.request.post(`${base}/pilots`, { ...json, data: { callsign } })
    ).json()) as { id: string };
    return p.id;
  };
  const aceId = await mkPilot(ACE);
  const beeId = await mkPilot(BEE);

  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [classId] } });
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [aceId, beeId] } });
  // Members carry an explicit channel each so the channels panel resolves a label too.
  await page.request.put(`${ev}/classes/${classId}/membership`, {
    ...json,
    data: {
      pilots: [
        { pilot: aceId, channel: 5658 },
        { pilot: beeId, channel: 5695 }
      ]
    }
  });
  // A FromRoster qual round: FillRound draws the two members into a single Scheduled heat. The
  // competitor refs are the pilot ids themselves (no CompetitorRegistered event) — the exact shape
  // that made callsigns fall back to raw ids pre-race.
  const round = (await (
    await page.request.post(`${ev}/rounds`, {
      ...json,
      data: {
        label: ROUND_LABEL,
        classes: [classId],
        format: 'timed_qual',
        params: {},
        win_condition: 'BestLap',
        seeding: 'FromRoster',
        channel_mode: 'Static'
      }
    })
  ).json()) as { id: string };
  expect(round.id).toBeTruthy();

  // Fill the round → a single heat is scheduled (NOT run). Capture its id so we can focus it.
  const fillAck = await page.request.post(`${ev}/control`, {
    ...json,
    data: { FillRound: { round: round.id } }
  });
  expect(fillAck.ok()).toBeTruthy();
  const heats = (await (await page.request.get(`${ev}/heats`)).json()) as Array<{
    heat: string;
    round?: string;
    phase: string;
  }>;
  const heat = heats.find((h) => h.round === round.id);
  expect(heat, 'the filled heat exists').toBeTruthy();
  expect(heat!.phase, 'the heat is scheduled — NOT run').toBe('Scheduled');

  // Focus this heat as the current heat (the shared worker Director may be pinned elsewhere).
  const focusAck = await page.request.post(`${ev}/control`, {
    ...json,
    data: { SetCurrentHeat: { heat: heat!.heat } }
  });
  expect(focusAck.ok()).toBeTruthy();

  // ── Into Live control: the lineup must show CALLSIGNS, not the raw pilot ids ───────────────────
  await page.goto('/');
  await enterPractice(page);
  await expect(page.locator('.conn-label')).toHaveText('live', { timeout: 15_000 });

  // The heat is Scheduled (not running) — the precise gap.
  await expect(page.locator('.phase').first()).toHaveText('Scheduled');

  const heatSheet = page.getByRole('region', { name: 'Heat sheet' });
  await expect(heatSheet.getByText(ACE, { exact: true })).toBeVisible({ timeout: 15_000 });
  await expect(heatSheet.getByText(BEE, { exact: true })).toBeVisible();
  // The raw pilot ids (the bug) never render.
  await expect(heatSheet.getByText(aceId)).toHaveCount(0);
  await expect(heatSheet.getByText(beeId)).toHaveCount(0);

  // The channels panel resolves the same callsigns (it lists one row per lineup competitor).
  const channels = page.getByRole('list', { name: 'Heat channels' });
  await expect(channels.getByText(ACE, { exact: true })).toBeVisible();
  await expect(channels.getByText(BEE, { exact: true })).toBeVisible();

  // ── Stage it (still NOT racing) — callsigns stay resolved ──────────────────────────────────────
  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Staged', { timeout: 15_000 });
  await expect(heatSheet.getByText(ACE, { exact: true })).toBeVisible();
  await expect(heatSheet.getByText(aceId)).toHaveCount(0);

  // ── Clean up the shared Director's event back to empty. ───────────────────────────────────────
  // Abort the staged heat back to Scheduled so the picker isn't left committed for the next spec.
  await page.request.post(`${ev}/control`, { ...json, data: { Abort: { heat: heat!.heat } } });
  await page.request.delete(`${ev}/rounds/${round.id}`);
  await page.request.put(`${ev}/classes/${classId}/membership`, { ...json, data: { pilots: [] } });
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [] } });
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
});
