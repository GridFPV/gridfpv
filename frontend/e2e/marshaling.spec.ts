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

// ── Slice 6: the full adjudication framework (DQ / points / throw-out / protests) ─────────────
//
// Drives the complete adjudication surface through the Marshaling screen against a real Director:
// file a protest, throw out a lap, DQ + reverse — asserting the AUDIT panel and the result
// snapshot re-fold at each step. Points deduction is driven over the control API and asserted on
// the class STANDINGS snapshot (points are season/event-level, not per-heat). The append→re-fold
// path is the load-bearing proof: every ruling is a recorded, reversible fact (marshaling.html §3).
const S6_PILOTS = ['Sol', 'Tama', 'Uma'];
const S6_HEAT = `marshal-s6-${Date.now()}`;

test('the full adjudication framework: protest, throw-out, DQ+reverse re-fold audit and standings', async ({
  page,
  director
}) => {
  await enterPractice(page);

  const control = (cmd: unknown) =>
    page.request.post(`${director.baseUrl}/events/practice/control`, {
      headers: { 'Content-Type': 'application/json' },
      data: cmd
    });

  expect((await control({ ScheduleHeat: { heat: S6_HEAT, lineup: S6_PILOTS } })).ok()).toBeTruthy();
  expect((await control({ SetCurrentHeat: { heat: S6_HEAT } })).ok()).toBeTruthy();

  await page.reload();
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.heat-id .value')).toHaveText(S6_HEAT, { timeout: 15_000 });
  await runToUnofficial(page);

  await page.getByRole('button', { name: /Marshaling/ }).click();
  const marshaling = page.getByRole('region', { name: 'Marshaling' });
  await expect(marshaling).toBeVisible();
  const audit = page.getByRole('complementary', { name: 'Audit trail' });
  await expect(marshaling.locator('button.lap').first()).toBeVisible({ timeout: 15_000 });

  const result = async () => {
    const res = await page.request.get(
      `${director.baseUrl}/events/practice/snapshot/heat/${S6_HEAT}?projection=result`
    );
    expect(res.ok()).toBeTruthy();
    return (await res.json()).body.HeatResult as {
      places: { competitor: { competitor: string }; laps: number; disqualified?: boolean }[];
    };
  };
  const lapsOf = (r: Awaited<ReturnType<typeof result>>, pilot: string) =>
    r.places.find((p) => p.competitor.competitor === pilot)?.laps ?? 0;

  // ── FILE A PROTEST against Sol → the audit gains a "Protest filed" entry ─────────────────────
  await marshaling.getByLabel('Protest competitor').selectOption(S6_PILOTS[0]);
  await marshaling.getByLabel('Protest note').fill('cut the course on lap 2');
  await marshaling.getByRole('button', { name: 'File protest' }).click();
  await expect(audit.getByText(/Protest filed/i).first()).toBeVisible({ timeout: 10_000 });

  // ── THROW OUT a lap → the audit gains "Thrown out" and the scored lap count drops ───────────
  const beforeThrow = await result();
  const firstLapPilot = S6_PILOTS.find((p) => lapsOf(beforeThrow, p) > 0)!;
  const beforeLaps = lapsOf(beforeThrow, firstLapPilot);
  // Select that pilot's first lap and throw it out (the throw-out targets the lap's end pass).
  const pilotCard = marshaling.locator('.comp', { hasText: firstLapPilot });
  await pilotCard.locator('button.lap').first().click();
  await marshaling.getByRole('button', { name: 'Throw out lap' }).click();
  await expect(audit.getByText(/Thrown out/i).first()).toBeVisible({ timeout: 10_000 });
  // The scored count for that pilot drops by one (the lap stays real, just uncounted).
  await expect
    .poll(async () => lapsOf(await result(), firstLapPilot), {
      timeout: 10_000,
      message: 'the throw-out should drop the scored lap count'
    })
    .toBe(beforeLaps - 1);

  // ── DQ a competitor, then REVERSE it — the result re-folds both ways ─────────────────────────
  await marshaling.getByLabel('Ruling competitor').selectOption(S6_PILOTS[1]);
  // Kind defaults to Disqualify; add a reason.
  await marshaling.getByLabel('DQ reason').fill('unsafe flying');
  await marshaling.getByRole('button', { name: 'Apply' }).click();
  await expect(audit.getByText(/DQ applied/i).first()).toBeVisible({ timeout: 10_000 });
  await expect
    .poll(
      async () =>
        (await result()).places.find((p) => p.competitor.competitor === S6_PILOTS[1])
          ?.disqualified ?? false,
      { timeout: 10_000, message: 'the DQ should re-fold the result' }
    )
    .toBe(true);
  // Reverse the DQ (it is offered in the generalized reverse-ruling list).
  await marshaling.getByLabel('Reverse ruling').selectOption({ index: 1 });
  await marshaling.getByRole('button', { name: 'Reverse ruling' }).click();
  await expect(audit.getByText(/Reversed/i).first()).toBeVisible({ timeout: 10_000 });
  await expect
    .poll(
      async () =>
        (await result()).places.find((p) => p.competitor.competitor === S6_PILOTS[1])
          ?.disqualified ?? false,
      { timeout: 10_000, message: 'reversing the DQ should re-fold the result back' }
    )
    .toBe(false);

  // ── DEDUCT POINTS over the control API → the audit shows it (points are standings-level) ─────
  expect(
    (await control({ DeductPoints: { heat: S6_HEAT, competitor: S6_PILOTS[2], points: 3 } })).ok()
  ).toBeTruthy();
  await expect(
    page
      .getByLabel('Marshaling')
      .getByText(/-3 points/i)
      .first()
  ).toBeVisible({
    timeout: 10_000
  });
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
  // Slice 6 mutating controls are equally hidden for a read-only pilot.
  await expect(marshaling.getByRole('button', { name: 'Throw out lap' })).toHaveCount(0);
  await expect(marshaling.getByRole('button', { name: 'File protest' })).toHaveCount(0);
  await expect(marshaling.getByRole('button', { name: 'Resolve protest' })).toHaveCount(0);
  // The audit panel is still present (read access is unrestricted).
  await expect(page.getByRole('complementary', { name: 'Audit trail' })).toBeVisible();
});

// ── Friendly names everywhere: lap headings + audit show callsigns, not raw pilot ids ─────────
//
// The Marshaling raw-id bug (#214 follow-up): the screen rendered the raw competitor refs (pilot
// ids for a roster-seeded heat) in the lap-list headings, the ruling/protest dropdowns, and the
// audit lines. This drives a real `FromRoster` heat of NAMED pilots (the refs are the pilot ids,
// distinct from the callsigns) through a DQ, and asserts the screen shows CALLSIGNS — never the
// pilot ids — in the lap headings, the ruling dropdown, and the composed audit line.
test('the Marshaling screen shows pilot callsigns (not raw ids) in lap headings + audit', async ({
  page,
  director
}) => {
  const base = director.baseUrl;
  const ev = `${base}/events/practice`;
  const json = { headers: { 'Content-Type': 'application/json' } };
  const SUFFIX = Date.now();
  const ACE = `E2E-Marsh-Ace-${SUFFIX}`;
  const BEE = `E2E-Marsh-Bee-${SUFFIX}`;
  const ROUND_LABEL = `E2E-MarshRound-${SUFFIX}`;

  // Open Class is pre-seeded; add two pilots, roster + a FromRoster qual round.
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

  // Fill the round → a single Scheduled heat whose competitor refs ARE the pilot ids.
  expect(
    (
      await page.request.post(`${ev}/control`, {
        ...json,
        data: { FillRound: { round: round.id } }
      })
    ).ok()
  ).toBeTruthy();
  const heats = (await (await page.request.get(`${ev}/heats`)).json()) as Array<{
    heat: string;
    round?: string;
  }>;
  const heat = heats.find((h) => h.round === round.id)!.heat;
  expect(
    (await page.request.post(`${ev}/control`, { ...json, data: { SetCurrentHeat: { heat } } })).ok()
  ).toBeTruthy();

  await enterPractice(page);
  await page.reload();
  // The heat is round-tagged → the header shows its friendly name (not the raw id).
  await expect(page.locator('.heat-id .value')).toHaveText(`${ROUND_LABEL} Heat 1`, {
    timeout: 15_000
  });

  // Run it to Unofficial so it has laps to marshal, then DQ a pilot.
  await runToUnofficial(page);

  await page.getByRole('button', { name: /Marshaling/ }).click();
  const marshaling = page.getByRole('region', { name: 'Marshaling' });
  await expect(marshaling).toBeVisible();
  await expect(marshaling.locator('button.lap').first()).toBeVisible({ timeout: 15_000 });

  // The Marshaling header shows the friendly heat name (not the raw heat id).
  await expect(marshaling.locator('.heat')).toContainText(`${ROUND_LABEL} Heat 1`);

  // The lap-list headings show CALLSIGNS — never the raw pilot ids.
  await expect(marshaling.locator('.comp h4', { hasText: ACE })).toBeVisible();
  await expect(marshaling.locator('.comp h4', { hasText: BEE })).toBeVisible();
  await expect(marshaling.getByText(aceId)).toHaveCount(0);
  await expect(marshaling.getByText(beeId)).toHaveCount(0);

  // The ruling dropdown labels are callsigns (the option value is still the ref the command targets).
  const ruling = marshaling.getByLabel('Ruling competitor');
  await expect(ruling.getByRole('option', { name: ACE })).toHaveCount(1);

  // DQ Ace via the ruling panel → the audit line composes the CALLSIGN, not the pilot id.
  await ruling.selectOption({ label: ACE });
  await marshaling.getByRole('button', { name: 'Apply' }).click();
  const audit = page.getByRole('complementary', { name: 'Audit trail' });
  await expect(audit.getByText(new RegExp(`${ACE}.*DQ applied`))).toBeVisible({ timeout: 10_000 });
  // The raw pilot id never appears in the audit line.
  await expect(audit.getByText(aceId)).toHaveCount(0);

  // ── Clean up the shared Director's event back to empty. ───────────────────────────────────────
  await page.request.post(`${ev}/control`, { ...json, data: { VoidHeat: { heat } } });
  await page.request.delete(`${ev}/rounds/${round.id}`);
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [] } });
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
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

// ── Slice 5: the provisional → official lifecycle + auto-official timer ───────────────────────
//
// The protest window is a per-round, OFF-by-default **auto-official timer**: when set, the runtime
// auto-finalizes (Unofficial → Final) once the window elapses from the race-end instant; the RD can
// always finalize early, and Revert re-opens a finalized result. These prove the four behaviours
// the slice promises end-to-end against a real Director:
//   • window ON  → the heat stays Unofficial (Provisional), then **auto-finalizes** after a short
//                  (test-set) window;
//   • the RD can **finalize early** (manual Finalize pre-empts the timer);
//   • **Revert** re-opens a finalized result to provisional;
//   • a **read-only** session sees the lifecycle but cannot finalize.
//
// The windowed heat needs a *round* carrying the protest window (a round-less free-text heat is
// `Off` by design), so this seeds a class + a `timed_qual` round with a short `After` window over
// the open REST API, then schedules the heat tagged with that round.

/** Run the focused current heat from Scheduled to Unofficial (Stage → Start → bank laps → ForceEnd). */
async function runToUnofficial(page: import('@playwright/test').Page): Promise<void> {
  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Staged');
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Running', { timeout: 15_000 });
  const lapCells = page.getByRole('region', { name: 'Heat sheet' }).locator('.laps');
  await expect
    .poll(
      async () =>
        (await lapCells.allTextContents()).reduce(
          (s, t) => s + (parseInt(t.match(/(\d+)/)?.[1] ?? '0', 10) || 0),
          0
        ),
      { timeout: 30_000, message: 'live laps should bank before ending the heat' }
    )
    .toBeGreaterThan(0);
  await page.getByRole('button', { name: 'ForceEnd', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Unofficial');
}

test('a configured protest window auto-finalizes the heat after the window elapses', async ({
  page,
  director
}) => {
  const post = (path: string, data: unknown) =>
    page.request.post(`${director.baseUrl}${path}`, {
      headers: { 'Content-Type': 'application/json' },
      data
    });
  const put = (path: string, data: unknown) =>
    page.request.put(`${director.baseUrl}${path}`, {
      headers: { 'Content-Type': 'application/json' },
      data
    });
  const control = (cmd: unknown) => post('/events/practice/control', cmd);

  // Seed a class + select it on Practice, then a `timed_qual` round with a SHORT protest window
  // (`After { micros }`) so the auto-official timer fires within the test, not minutes later.
  const classRes = await post('/classes', { name: `protest-${Date.now()}` });
  expect(classRes.ok()).toBeTruthy();
  const classId = (await classRes.json()).id as string;
  expect((await put('/events/practice/classes', { ids: [classId] })).ok()).toBeTruthy();

  const roundRes = await post('/events/practice/rounds', {
    label: 'Protest Qual',
    classes: [classId],
    format: 'timed_qual',
    params: { rounds: '1' },
    win_condition: { Timed: { window_micros: 60_000_000 } },
    // A ~6s window — long enough to reliably observe "Provisional" + the countdown first, short
    // enough that the auto-finalize lands well within the test's 15s wait for Final.
    protest_window: { After: { micros: 6_000_000 } }
  });
  expect(roundRes.ok()).toBeTruthy();
  const roundId = (await roundRes.json()).id as string;

  const heat = `marshal-protest-${Date.now()}`;
  expect(
    (await control({ ScheduleHeat: { heat, lineup: UI_PILOTS, round: roundId } })).ok()
  ).toBeTruthy();
  expect((await control({ SetCurrentHeat: { heat } })).ok()).toBeTruthy();

  await enterPractice(page);
  await page.reload();
  // The heat is round-tagged, so the header shows its friendly "<Round> Heat N" name (not the raw id).
  await expect(page.locator('.heat-id .value')).toHaveText('Protest Qual Heat 1', {
    timeout: 15_000
  });

  await runToUnofficial(page);

  // The lifecycle banner shows **Provisional** with a live auto-official countdown (the runtime
  // armed the window and logged its deadline; the console counts down to it).
  const lifecycle = page.getByRole('status', { name: 'Result lifecycle' });
  await expect(lifecycle).toContainText(/Provisional/i, { timeout: 10_000 });
  await expect(lifecycle).toContainText(/auto-official in/i);

  // …and the runtime AUTO-FINALIZES once the window elapses: the heat folds to Final on its own,
  // with no RD action. (The banner flips to Official.)
  await expect(page.locator('.phase').first()).toHaveText('Final', { timeout: 15_000 });
  await expect(lifecycle).toContainText(/Official/i);
});

test('the RD can finalize early, and Revert re-opens the result to provisional', async ({
  page,
  director
}) => {
  // No protest window needed for early-finalize / revert: a round-less heat is Provisional (manual
  // finalize only), which is exactly the path this exercises through the open control path.
  const control = (cmd: unknown) =>
    page.request.post(`${director.baseUrl}/events/practice/control`, {
      headers: { 'Content-Type': 'application/json' },
      data: cmd
    });
  const heat = `marshal-early-${Date.now()}`;
  expect((await control({ ScheduleHeat: { heat, lineup: UI_PILOTS } })).ok()).toBeTruthy();
  expect((await control({ SetCurrentHeat: { heat } })).ok()).toBeTruthy();

  await enterPractice(page);
  await page.reload();
  await expect(page.locator('.heat-id .value')).toHaveText(heat, { timeout: 15_000 });

  await runToUnofficial(page);

  // Provisional (no window armed) — manual finalize only.
  const lifecycle = page.getByRole('status', { name: 'Result lifecycle' });
  await expect(lifecycle).toContainText(/Provisional/i, { timeout: 10_000 });

  // ── Finalize EARLY: the RD presses Finalize → the result becomes Official immediately ──────────
  await page.getByRole('button', { name: 'Finalize', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Final', { timeout: 10_000 });
  await expect(lifecycle).toContainText(/Official/i);

  // ── REVERT re-opens it to provisional (correctable again) ──────────────────────────────────────
  // Revert is a destructive off-ramp, so it confirms before firing (ConfirmButton two-step).
  await page.getByRole('button', { name: 'Revert', exact: true }).click();
  await page.getByRole('button', { name: 'Confirm' }).click();
  await expect(page.locator('.phase').first()).toHaveText('Unofficial', { timeout: 10_000 });
  await expect(lifecycle).toContainText(/Provisional/i);
});

test('a read-only session sees the lifecycle but cannot finalize', async ({ page, director }) => {
  // Schedule + run a heat to Unofficial as the open RD, then re-enter read-only and confirm the
  // lifecycle is visible while the Finalize transition is not available to a pilot.
  const control = (cmd: unknown) =>
    page.request.post(`${director.baseUrl}/events/practice/control`, {
      headers: { 'Content-Type': 'application/json' },
      data: cmd
    });
  const heat = `marshal-ro-life-${Date.now()}`;
  expect((await control({ ScheduleHeat: { heat, lineup: UI_PILOTS } })).ok()).toBeTruthy();
  expect((await control({ SetCurrentHeat: { heat } })).ok()).toBeTruthy();

  await enterPractice(page);
  await page.reload();
  await expect(page.locator('.heat-id .value')).toHaveText(heat, { timeout: 15_000 });
  await runToUnofficial(page);

  // Re-enter as a read-only pilot via the role seam.
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
  await liveNav.click();

  // The lifecycle is visible to the read-only pilot (it surfaces the governance state)…
  await expect(page.locator('.phase').first()).toHaveText('Unofficial', { timeout: 15_000 });
  await expect(page.getByRole('status', { name: 'Result lifecycle' })).toContainText(
    /Provisional/i
  );
  // …but the heat transitions are hidden for a pilot (read-only): no Finalize / Revert controls.
  await expect(page.getByRole('group', { name: 'Heat transitions' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Finalize', exact: true })).toHaveCount(0);
});
