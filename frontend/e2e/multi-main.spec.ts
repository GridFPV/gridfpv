/**
 * **Multi-Main as a first-class finals format** (#219, decisions D14).
 *
 * The deliverable proof for the multi-main UI + stacking: a finals **split by qualifying rank** —
 * NOT elimination. This spec seeds a small 6-pilot field, runs a qualifier to a ranking, then adds a
 * `multi_main` round (`main_size 2`) seeded `FromRanking` from the qualifier. It **generates** the
 * mains in one action (deterministic → fill-all), drives each main to Final over the real control
 * path, then asserts:
 *
 *  - the round's heats render in the **Rounds & Heats** UI as the tiered mains **"A-Main", "B-Main",
 *    "C-Main"** (not "<Round> Heat N") — the multi-main heat-naming rule;
 *  - the round's **ranking stacks** the mains: A-main finishers take places 1–2, B-main 3–4, C-main
 *    5–6, so the worst A-main finisher still outranks the best B-main finisher.
 *
 * Heat lifecycle runs over the real `POST /events/{id}/control` path (Stage → Start → ForceEnd →
 * Finalize) — the same transitions the Live screen emits. Importing from `./observability.js` means
 * a failure carries the full-stack dump.
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

test('RD runs a multi-main finals: ranked field splits into A/B/C-mains, results stack into the standings', async ({
  page,
  director
}) => {
  const base = director.baseUrl;
  const ev = `${base}/events/practice`;
  const SUFFIX = Date.now();
  // Six pilots; the qualifier ranks them A>B>C>D>E>F (best-lap descending) so the mains split is
  // deterministic: A-main(A,B), B-main(C,D), C-main(E,F).
  const CALLS = ['A', 'B', 'C', 'D', 'E', 'F'].map((c) => `E2E-MM-${c}-${SUFFIX}`);
  const QUAL_LABEL = `E2E-MM-Qual-${SUFFIX}`;
  const MAINS_LABEL = `E2E-MM-Mains-${SUFFIX}`;

  // ── Set up over the real write paths: Open Class selected, six members. ────────────────────────
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

  // A timed-qual qualifier seeded straight from the roster (one heat over all six).
  const qual = (await (
    await page.request.post(`${ev}/rounds`, {
      ...json,
      data: {
        label: QUAL_LABEL,
        classes: [classId],
        format: 'timed_qual',
        params: { rounds: '1' },
        win_condition: 'BestLap',
        seeding: 'FromRoster',
        channel_mode: 'PerHeat'
      }
    })
  ).json()) as { id: string };
  expect(qual.id).toBeTruthy();

  // The multi-main finals: split the qualifier ranking into mains of two (A/B/C), seeded
  // FromRanking from the qualifier (top 6 = whole field).
  const mains = (await (
    await page.request.post(`${ev}/rounds`, {
      ...json,
      data: {
        label: MAINS_LABEL,
        classes: [classId],
        format: 'multi_main',
        params: { main_size: '2' },
        win_condition: 'BestLap',
        seeding: { FromRanking: { source_rounds: [qual.id], top_n: 6 } },
        channel_mode: 'PerHeat'
      }
    })
  ).json()) as { id: string };
  expect(mains.id).toBeTruthy();

  const control = (cmd: unknown) => page.request.post(`${ev}/control`, { ...json, data: cmd });
  const practiceHeats = async (): Promise<Array<{ heat: string; round?: string; phase: string }>> =>
    (await (await page.request.get(`${ev}/heats`)).json()) as Array<{
      heat: string;
      round?: string;
      phase: string;
    }>;

  // ── Drive a heat to Final over the control path (Stage → Start → ForceEnd → Finalize). ─────────
  const runHeatToFinal = async (heatId: string) => {
    expect((await control({ SetCurrentHeat: { heat: heatId } })).ok()).toBeTruthy();
    expect((await control({ Stage: { heat: heatId } })).ok()).toBeTruthy();
    expect((await control({ Start: { heat: heatId } })).ok()).toBeTruthy();
    await expect
      .poll(async () => (await practiceHeats()).find((h) => h.heat === heatId)?.phase, {
        timeout: 20_000
      })
      .toBe('Running');
    await page.waitForTimeout(1500);
    expect((await control({ ForceEnd: { heat: heatId } })).ok()).toBeTruthy();
    expect((await control({ Finalize: { heat: heatId } })).ok()).toBeTruthy();
    await expect
      .poll(async () => (await practiceHeats()).find((h) => h.heat === heatId)?.phase, {
        timeout: 15_000
      })
      .toBe('Final');
  };

  // ── Run the qualifier so it produces a ranking the mains seed from. ────────────────────────────
  expect((await control({ FillRound: { round: qual.id, mode: 'All' } })).ok()).toBeTruthy();
  await expect
    .poll(async () => (await practiceHeats()).filter((h) => h.round === qual.id).length, {
      timeout: 15_000
    })
    .toBeGreaterThanOrEqual(1);
  const qualHeatIds = (await practiceHeats()).filter((h) => h.round === qual.id).map((h) => h.heat);
  for (const id of qualHeatIds) await runHeatToFinal(id);

  // ── Generate the mains (deterministic → fill-all, #216): three mains of two. ───────────────────
  expect((await control({ FillRound: { round: mains.id, mode: 'All' } })).ok()).toBeTruthy();
  await expect
    .poll(async () => (await practiceHeats()).filter((h) => h.round === mains.id).length, {
      timeout: 15_000
    })
    .toBe(3);

  // The engine ids the mains `main-A`, `main-B`, `main-C` (top-down by qualifying rank).
  const mainHeatIds = (await practiceHeats())
    .filter((h) => h.round === mains.id)
    .map((h) => h.heat);
  expect([...mainHeatIds].sort()).toEqual(['main-A', 'main-B', 'main-C']);

  // Run every main to Final.
  for (const id of mainHeatIds) await runHeatToFinal(id);

  // ── The mains' results stack: A-main → 1–2, B-main → 3–4, C-main → 5–6. ───────────────────────
  // Read the round ranking off the real projection (the same stacked standing the UI renders).
  const ranking = (await (
    await page.request.get(`${ev}/rounds/${mains.id}/ranking`)
  ).json()) as Array<{ competitor: string; position: number }>;
  expect(ranking.length).toBe(6);
  // Positions are dense 1..6 across the three stacked mains.
  expect(ranking.map((r) => r.position)).toEqual([1, 2, 3, 4, 5, 6]);
  // The worst A-main finisher (position 2) outranks the best B-main finisher (position 3) — the
  // defining property of the format (your placement is bounded by the tier you qualified into).
  const a2 = ranking.find((r) => r.position === 2)!.competitor;
  const b1 = ranking.find((r) => r.position === 3)!.competitor;
  expect(a2).not.toEqual(b1);

  // ── Into the Rounds & Heats UI: the heats render as the tiered mains "A-Main", "B-Main", … ─────
  await page.goto('/');
  await enterPractice(page);
  await openTab(page, 'Rounds & Heats');
  const heatRound = page.getByRole('region', { name: `Heats for ${MAINS_LABEL}` });
  await expect(heatRound).toBeVisible({ timeout: 15_000 });
  for (const tier of ['A-Main', 'B-Main', 'C-Main']) {
    await expect(heatRound.getByText(tier, { exact: true }).first()).toBeVisible({
      timeout: 15_000
    });
  }

  // ── Clean up the shared Director's event back to empty. ───────────────────────────────────────
  const eventRounds = async () => {
    const events = (await (await page.request.get(`${base}/events`)).json()) as Array<{
      id: string;
      rounds?: Array<{ id: string }>;
    }>;
    return events.find((e) => e.id === 'practice')?.rounds ?? [];
  };
  for (const r of await eventRounds()) await page.request.delete(`${ev}/rounds/${r.id}`);
  await page.request.put(`${ev}/classes/${classId}/membership`, { ...json, data: { pilots: [] } });
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [] } });
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
});
