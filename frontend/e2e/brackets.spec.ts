/**
 * **Single-elimination bracket — level-per-round advance + visualization** (#217, decisions D13).
 *
 * The deliverable proof for the bracket UI: a bracket is a **chain of rounds**, one per level. This
 * spec seeds a small 4-pilot single-elim (a 2-heat **Semifinals** level seeded `FromRoster`), drives
 * both semis heats to Final over the real control path, then in the **Rounds & Heats** UI clicks
 * **Advance bracket** — asserting it creates the next level seeded `FromHeatWinners` and generates a
 * one-heat **Final**, that the bracket progresses to that one-heat final, and that the chain renders
 * as a coherent **BracketTree** (level columns) on the root level.
 *
 * Heat lifecycle is driven over the real `POST /events/{id}/control` path (Stage → Start → ForceEnd →
 * Finalize) — the same transitions the Live screen emits — so the spec exercises the bracket-specific
 * UI (advance + visualization) without re-driving the whole Live loop three times. Importing from
 * `./observability.js` means a failure carries the full-stack dump.
 */
import { expect, test } from './observability.js';
import type { Page } from '@playwright/test';

const json = { headers: { 'Content-Type': 'application/json' } };

/** Get the shared worker Director into the Practice event's workspace. */
async function enterPractice(page: Page) {
  const liveNav = page.getByRole('button', { name: /Live control/ });
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

async function openTab(page: Page, name: string) {
  await page.getByRole('navigation', { name: 'Screens' }).getByRole('button', { name }).click();
}

test('RD runs a single-elim through its levels: semis → one-heat final, rendered as a bracket', async ({
  page,
  director
}) => {
  const base = director.baseUrl;
  const ev = `${base}/events/practice`;
  const SUFFIX = Date.now();
  const CALLS = ['A', 'B', 'C', 'D'].map((c) => `E2E-Brk-${c}-${SUFFIX}`);
  const SEMIS_LABEL = `E2E-Semis-${SUFFIX}`;

  // ── Set up over the real write paths: Open Class selected, four members, a single_elim level-1 ──
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
  const pilotIds: string[] = [];
  for (const c of CALLS) pilotIds.push(await mkPilot(c));

  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [classId] } });
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: pilotIds } });
  await page.request.put(`${ev}/classes/${classId}/membership`, {
    ...json,
    data: { pilots: pilotIds }
  });

  // A single_elim level-1 round seeded straight from the 4-member roster (heat_size 2 → two
  // semifinal heats). This is the bracket chain's ROOT level — every later level chains off it via
  // FromHeatWinners.
  const semis = (await (
    await page.request.post(`${ev}/rounds`, {
      ...json,
      data: {
        label: SEMIS_LABEL,
        classes: [classId],
        format: 'single_elim',
        params: { heat_size: '2' },
        win_condition: 'BestLap',
        seeding: 'FromRoster',
        channel_mode: 'PerHeat'
      }
    })
  ).json()) as { id: string };
  expect(semis.id).toBeTruthy();

  const control = (cmd: unknown) => page.request.post(`${ev}/control`, { ...json, data: cmd });
  const practiceHeats = async (): Promise<Array<{ heat: string; round?: string; phase: string }>> =>
    (await (await page.request.get(`${ev}/heats`)).json()) as Array<{
      heat: string;
      round?: string;
      phase: string;
    }>;

  // Generate the semis heats (a bracket is deterministic → fill-all, #216).
  expect((await control({ FillRound: { round: semis.id, mode: 'All' } })).ok()).toBeTruthy();
  await expect
    .poll(async () => (await practiceHeats()).filter((h) => h.round === semis.id).length, {
      timeout: 15_000
    })
    .toBe(2);

  // ── Drive each semis heat to Final over the control path (Stage → Start → ForceEnd → Finalize) ──
  const runHeatToFinal = async (heatId: string) => {
    expect((await control({ SetCurrentHeat: { heat: heatId } })).ok()).toBeTruthy();
    expect((await control({ Stage: { heat: heatId } })).ok()).toBeTruthy();
    expect((await control({ Start: { heat: heatId } })).ok()).toBeTruthy();
    // The runtime auto-advances Armed → Running after a randomized hold; wait for Running, then let
    // the sim bank a couple of laps so the scored result is real, then end + finalize.
    await expect
      .poll(async () => (await practiceHeats()).find((h) => h.heat === heatId)?.phase, {
        timeout: 20_000
      })
      .toBe('Running');
    // Give the fast sim time to emit laps before closing the window.
    await page.waitForTimeout(1500);
    expect((await control({ ForceEnd: { heat: heatId } })).ok()).toBeTruthy();
    expect((await control({ Finalize: { heat: heatId } })).ok()).toBeTruthy();
    await expect
      .poll(async () => (await practiceHeats()).find((h) => h.heat === heatId)?.phase, {
        timeout: 15_000
      })
      .toBe('Final');
  };

  const semiHeatIds = (await practiceHeats())
    .filter((h) => h.round === semis.id)
    .map((h) => h.heat);
  expect(semiHeatIds.length).toBe(2);
  for (const id of semiHeatIds) await runHeatToFinal(id);

  // ── Into the Rounds & Heats UI: the chain renders as a BracketTree on the root level ──────────
  await page.goto('/');
  await enterPractice(page);
  await openTab(page, 'Rounds & Heats');
  const heatRound = page.getByRole('region', { name: `Heats for ${SEMIS_LABEL}` });
  await expect(heatRound).toBeVisible({ timeout: 15_000 });

  // The bracket panel renders under the root level, showing the Semifinals level column.
  const bracket = page.getByLabel(`Bracket — ${SEMIS_LABEL}`);
  await expect(bracket).toBeVisible({ timeout: 15_000 });
  await expect(bracket.getByText(SEMIS_LABEL).first()).toBeVisible();

  // ── Advance the bracket: both semis are Final → create the next level (the Final) + fill it ────
  await heatRound.getByRole('button', { name: 'Advance bracket' }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();

  // A next-level round was created on the Director, seeded FromHeatWinners of the semis, holding a
  // single heat (the one-heat Final — the chain progressed to its final).
  const bracketLevels = async () => {
    const events = (await (await page.request.get(`${base}/events`)).json()) as Array<{
      id: string;
      rounds?: Array<{ id: string; label: string; seeding: unknown }>;
    }>;
    return events.find((e) => e.id === 'practice')?.rounds ?? [];
  };
  const findFinalLevel = async () =>
    (await bracketLevels()).find((r) => {
      const seed = r.seeding as { FromHeatWinners?: { source_round: string } };
      return seed?.FromHeatWinners?.source_round === semis.id;
    });
  await expect.poll(async () => Boolean(await findFinalLevel()), { timeout: 15_000 }).toBe(true);
  const finalLevel = await findFinalLevel();
  expect(finalLevel).toBeTruthy();

  // The Final level holds exactly one heat — the bracket progressed to a one-heat final.
  await expect
    .poll(async () => (await practiceHeats()).filter((h) => h.round === finalLevel!.id).length, {
      timeout: 15_000
    })
    .toBe(1);

  // The Final level lists in the Heats UI with its single heat, and the bracket view now stitches
  // BOTH levels (Semifinals + Final) into the chain on the root level.
  const finalRound = page.getByRole('region', { name: `Heats for ${finalLevel!.label}` });
  await expect(finalRound).toBeVisible({ timeout: 15_000 });
  await expect(finalRound.locator('.heat-row')).toHaveCount(1);
  await expect(bracket.getByText(finalLevel!.label).first()).toBeVisible({ timeout: 15_000 });
  if (process.env.GRIDFPV_SHOTS)
    await bracket.screenshot({ path: `${process.env.GRIDFPV_SHOTS}/bracket-chain.png` });

  // ── Clean up the shared Director's event back to empty. ───────────────────────────────────────
  for (const r of await bracketLevels()) await page.request.delete(`${ev}/rounds/${r.id}`);
  await page.request.put(`${ev}/classes/${classId}/membership`, { ...json, data: { pilots: [] } });
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [] } });
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
});
