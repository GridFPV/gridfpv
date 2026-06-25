/**
 * Marshaling correction over a real Director (Marshaling Slice 2 — correction primitives).
 *
 * The UI for marshaling is Slice 3; this slice's proof drives the **control endpoint** directly,
 * mirroring how `race.spec.ts` schedules a heat over the open control path. The shape of the proof:
 *
 *   1. Schedule + run a heat to **Unofficial** (the provisional state corrections apply in).
 *   2. Read the heat's **result** snapshot — every pilot placed, none disqualified.
 *   3. Apply **one correction** over the control API — an `ApplyPenalty { Disqualify }` of the leader.
 *      (A DQ needs no log offset, so it is the correction an e2e can drive without a raw-log read;
 *      the offset-addressed primitives — void / adjust / split / reverse — are covered by the Rust
 *      unit + projection + scoring tests, which assert the fold exactly.)
 *   4. Read the result snapshot **again** and assert it **re-folded**: the DQ'd leader is now flagged
 *      `disqualified` and sunk below the field. The re-fold is the load-bearing assertion — the
 *      result is a pure function of the appended log, so a single appended correction recomputes the
 *      standings deterministically (architecture.html §3 / marshaling.html §4).
 *
 * Like `race.spec.ts`, this imports `test`/`expect` from `./observability.js`, so a failure carries
 * the full-stack dump (browser console, WS frames, Director log). The Director is booted with no
 * token, so control is open.
 */
import { expect, test } from './observability.js';

const PILOTS = ['Mar', 'Nor', 'Ortega'];
// Unique per run so the shared worker Director (which accumulates events across specs) never
// collides this heat with a leftover from another spec.
const HEAT_ID = `marshal-${Date.now()}`;

test('a marshaling DQ correction re-folds the heat result', async ({ page, director }) => {
  await page.goto('/');

  // Enter Practice (handle the active-event-resume / picker branches like race.spec.ts).
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
  const result = async () => {
    const res = await page.request.get(
      `${director.baseUrl}/events/practice/snapshot/heat/${HEAT_ID}?projection=result`
    );
    expect(res.ok()).toBeTruthy();
    return (await res.json()).body.HeatResult as {
      places: { competitor: { competitor: string }; position: number; disqualified?: boolean }[];
    };
  };
  // `disqualified` is omitted from the wire when false (a clean result carries no extra bytes),
  // so coalesce its absence to `false`.
  const placeOf = (r: Awaited<ReturnType<typeof result>>, pilot: string) => {
    const p = r.places.find((x) => x.competitor.competitor === pilot);
    if (!p) throw new Error(`pilot ${pilot} not in result: ${JSON.stringify(r.places)}`);
    return { position: p.position, disqualified: p.disqualified ?? false };
  };

  // ── Schedule + focus the heat, then run it to Unofficial ──────────────────────────────
  expect((await control({ ScheduleHeat: { heat: HEAT_ID, lineup: PILOTS } })).ok()).toBeTruthy();
  expect((await control({ SetCurrentHeat: { heat: HEAT_ID } })).ok()).toBeTruthy();

  await page.reload();
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.heat-id .value')).toHaveText(HEAT_ID, { timeout: 15_000 });

  // Stage → Start; the runtime auto-advances Armed → Running, then ForceEnd to Unofficial.
  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Staged');
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Running', { timeout: 15_000 });

  // Let some laps bank so every pilot is in the result, then end the window.
  const heatSheet = page.getByRole('region', { name: 'Heat sheet' });
  const lapCells = heatSheet.locator('.laps');
  await expect
    .poll(
      async () =>
        (await lapCells.allTextContents()).reduce(
          (s, t) => s + (parseInt(t.match(/(\d+)/)?.[1] ?? '0', 10) || 0),
          0
        ),
      { timeout: 30_000, message: 'live laps should appear before ending the heat' }
    )
    .toBeGreaterThan(0);
  await page.getByRole('button', { name: 'ForceEnd', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Unofficial');

  // ── Baseline: every pilot placed, none disqualified ───────────────────────────────────
  const before = await result();
  expect(before.places.length).toBe(PILOTS.length);
  for (const pilot of PILOTS) {
    expect(placeOf(before, pilot).disqualified).toBe(false);
  }

  // ── Apply ONE correction over the control API: disqualify the current leader ───────────
  const leader = before.places.find((p) => p.position === 1)!.competitor.competitor;
  expect(
    (
      await control({
        ApplyPenalty: { heat: HEAT_ID, competitor: leader, penalty: 'Disqualify' }
      })
    ).ok()
  ).toBeTruthy();

  // ── The result RE-FOLDS: the DQ'd leader is flagged and sunk below the field ───────────
  await expect
    .poll(async () => placeOf(await result(), leader).disqualified, {
      timeout: 10_000,
      message: 'the DQ correction should re-fold the result'
    })
    .toBe(true);
  const after = await result();
  // A disqualified competitor ranks after every non-disqualified one — the standings re-folded.
  const dqPos = placeOf(after, leader).position;
  for (const p of after.places) {
    if (!p.disqualified) expect(p.position).toBeLessThan(dqPos);
  }
});
