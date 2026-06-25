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

// ── Slice 3: the Marshaling UI drives corrections through the screen, against a real Director ──
const UI_PILOTS = ['Pax', 'Quill', 'Rune'];
const UI_HEAT = `marshal-ui-${Date.now()}`;

/** Enter Practice and confirm the live read stream is up (mirrors the helper above). */
async function enterPractice(page: import('@playwright/test').Page): Promise<void> {
  await page.goto('/');
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
}

test('the Marshaling screen corrects laps and the audit + standings update live', async ({
  page,
  director
}) => {
  await enterPractice(page);

  const control = (cmd: unknown) =>
    page.request.post(`${director.baseUrl}/events/practice/control`, {
      headers: { 'Content-Type': 'application/json' },
      data: cmd
    });

  // Schedule + focus a heat, run it to Unofficial so it has laps to marshal.
  expect((await control({ ScheduleHeat: { heat: UI_HEAT, lineup: UI_PILOTS } })).ok()).toBeTruthy();
  expect((await control({ SetCurrentHeat: { heat: UI_HEAT } })).ok()).toBeTruthy();

  await page.reload();
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.heat-id .value')).toHaveText(UI_HEAT, { timeout: 15_000 });

  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Staged');
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Running', { timeout: 15_000 });

  const heatSheet = page.getByRole('region', { name: 'Heat sheet' });
  const lapCells = heatSheet.locator('.laps');
  await expect
    .poll(
      async () =>
        (await lapCells.allTextContents()).reduce(
          (s, t) => s + (parseInt(t.match(/(\d+)/)?.[1] ?? '0', 10) || 0),
          0
        ),
      { timeout: 30_000, message: 'live laps should bank before marshaling' }
    )
    .toBeGreaterThan(1);
  await page.getByRole('button', { name: 'ForceEnd', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Unofficial');

  // ── Open the Marshaling screen ───────────────────────────────────────────────────────
  await page.getByRole('button', { name: /Marshaling/ }).click();
  const marshaling = page.getByRole('region', { name: 'Marshaling' });
  await expect(marshaling).toBeVisible();
  const audit = page.getByRole('complementary', { name: 'Audit trail' });

  // A lap shows up in the list once the snapshot loads.
  await expect(marshaling.locator('button.lap').first()).toBeVisible({ timeout: 15_000 });
  const correctionTime = marshaling.getByLabel('Correction time');

  // Select the first lap and confirm it took (a re-fold from a prior correction can briefly clear
  // the selection, so select-then-confirm via aria-pressed is the robust pattern). Re-click if the
  // first click landed during a re-render.
  const selectFirstLap = async () => {
    const lap = marshaling.locator('button.lap').first();
    await expect(async () => {
      // Click only when not already selected (the lap button toggles), then confirm it took.
      if ((await lap.getAttribute('aria-pressed')) !== 'true') await lap.click();
      await expect(lap).toHaveAttribute('aria-pressed', 'true', { timeout: 2_000 });
    }).toPass({ timeout: 15_000 });
  };

  // ── Select a lap and EDIT its time — the audit gains a "Re-timed" entry ───────────────
  await selectFirstLap();
  await correctionTime.fill('1.234');
  await marshaling.getByRole('button', { name: 'Edit time' }).click();
  await expect(audit.getByText(/Re-timed/i).first()).toBeVisible({ timeout: 10_000 });

  // ── Select a lap and SPLIT it — the audit gains a "Split" entry ───────────────────────
  await selectFirstLap();
  await correctionTime.fill('0.500');
  await marshaling.getByRole('button', { name: 'Split' }).click();
  await expect(audit.getByText(/Split/i).first()).toBeVisible({ timeout: 10_000 });

  // ── DQ a competitor through the ruling panel ─────────────────────────────────────────
  await marshaling.getByLabel('Ruling competitor').selectOption(UI_PILOTS[0]);
  // Kind defaults to Disqualify.
  await marshaling.getByRole('button', { name: 'Apply' }).click();
  await expect(audit.getByText(/DQ applied/i).first()).toBeVisible({ timeout: 10_000 });

  // The DQ flows into standings (the result snapshot re-folds).
  const result = async () => {
    const res = await page.request.get(
      `${director.baseUrl}/events/practice/snapshot/heat/${UI_HEAT}?projection=result`
    );
    expect(res.ok()).toBeTruthy();
    return (await res.json()).body.HeatResult as {
      places: { competitor: { competitor: string }; disqualified?: boolean }[];
    };
  };
  await expect
    .poll(
      async () =>
        (await result()).places.find((p) => p.competitor.competitor === UI_PILOTS[0])
          ?.disqualified ?? false,
      { timeout: 10_000, message: 'the UI DQ should re-fold the standings' }
    )
    .toBe(true);

  // ── REVERSE the DQ through the audit-backed reverse control — the audit gains "Reversed" ─
  await marshaling.getByLabel('Reverse ruling').selectOption({ index: 1 });
  await marshaling.getByRole('button', { name: 'Reverse ruling' }).click();
  await expect(audit.getByText(/Reversed/i).first()).toBeVisible({ timeout: 10_000 });
  // And the DQ is undone in the standings.
  await expect
    .poll(
      async () =>
        (await result()).places.find((p) => p.competitor.competitor === UI_PILOTS[0])
          ?.disqualified ?? false,
      { timeout: 10_000, message: 'reversing the DQ should re-fold the standings back' }
    )
    .toBe(false);
});

test('a read-only session sees the laps + audit but cannot mutate', async ({ page, director }) => {
  // Reuse the heat the UI test created if present, else schedule a fresh one so the screen has laps.
  const roHeat = `marshal-ro-${Date.now()}`;
  const control = (cmd: unknown) =>
    page.request.post(`${director.baseUrl}/events/practice/control`, {
      headers: { 'Content-Type': 'application/json' },
      data: cmd
    });
  expect((await control({ ScheduleHeat: { heat: roHeat, lineup: UI_PILOTS } })).ok()).toBeTruthy();
  expect((await control({ SetCurrentHeat: { heat: roHeat } })).ok()).toBeTruthy();

  // Enter as a read-only pilot via the role seam.
  await page.goto('/?role=readonly');
  const liveNav = page.getByRole('button', { name: /Live control/ });
  await page.getByRole('button', { name: /Events/ }).click();
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
  await expect(liveNav).toBeVisible({ timeout: 15_000 });

  await page.getByRole('button', { name: /Marshaling/ }).click();
  const marshaling = page.getByRole('region', { name: 'Marshaling' });
  await expect(marshaling).toBeVisible();
  // The read-only banner shows, and NO mutating controls are present.
  await expect(marshaling.getByText(/Read-only/i)).toBeVisible();
  await expect(marshaling.getByRole('button', { name: 'Remove (void)' })).toHaveCount(0);
  await expect(marshaling.getByRole('button', { name: 'Apply' })).toHaveCount(0);
  await expect(marshaling.getByRole('button', { name: 'Void heat' })).toHaveCount(0);
  // The audit panel is still present (read access is unrestricted).
  await expect(page.getByRole('complementary', { name: 'Audit trail' })).toBeVisible();
});

// ── Slice 4: the signal-as-evidence RSSI graph ──────────────────────────────────────────────
//
// The graph mounts only for a heat that captured an RSSI trace (a RotorHazard heat). The shared
// e2e Director runs the **Mock/sim timer**, which emits no signal facts — so a heat run here has
// **no trace** and the screen must take the lap-only **sim fallback** (no graph). That fallback is
// the half this open-Director e2e can prove end-to-end. The *trace-present* half — the graph
// drawing the line/thresholds/markers and a marker-click selecting the lap — is driven against a
// seeded `SignalTraceView` in the component tests (`MarshalingScreen.test.ts`, Slice 4) and against
// real RSSI in the dockerized-RotorHazard harness (`crates/adapters/tests/rh_signal.rs`,
// `cargo xtask live`), because injecting signal facts needs the RH adapter, not the control API.
const SIM_HEAT = `marshal-sim-${Date.now()}`;

test('a sim heat (no captured trace) shows no RSSI graph and keeps the lap-only layout', async ({
  page,
  director
}) => {
  await enterPractice(page);

  const control = (cmd: unknown) =>
    page.request.post(`${director.baseUrl}/events/practice/control`, {
      headers: { 'Content-Type': 'application/json' },
      data: cmd
    });

  // Schedule + focus a Mock heat and run it to Unofficial so it has laps to marshal.
  expect(
    (await control({ ScheduleHeat: { heat: SIM_HEAT, lineup: UI_PILOTS } })).ok()
  ).toBeTruthy();
  expect((await control({ SetCurrentHeat: { heat: SIM_HEAT } })).ok()).toBeTruthy();

  await page.reload();
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.heat-id .value')).toHaveText(SIM_HEAT, { timeout: 15_000 });

  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Staged');
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Running', { timeout: 15_000 });

  const heatSheet = page.getByRole('region', { name: 'Heat sheet' });
  const lapCells = heatSheet.locator('.laps');
  await expect
    .poll(
      async () =>
        (await lapCells.allTextContents()).reduce(
          (s, t) => s + (parseInt(t.match(/(\d+)/)?.[1] ?? '0', 10) || 0),
          0
        ),
      { timeout: 30_000, message: 'live laps should bank before marshaling' }
    )
    .toBeGreaterThan(0);
  await page.getByRole('button', { name: 'ForceEnd', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Unofficial');

  await page.getByRole('button', { name: /Marshaling/ }).click();
  const marshaling = page.getByRole('region', { name: 'Marshaling' });
  await expect(marshaling).toBeVisible();

  // The lap list (the sim fallback) renders…
  await expect(marshaling.locator('button.lap').first()).toBeVisible({ timeout: 15_000 });
  // …but the RSSI graph does NOT mount — no trace was captured for a Mock heat.
  await expect(page.getByLabel('RSSI signal graph')).toHaveCount(0);
});
