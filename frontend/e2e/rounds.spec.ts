/**
 * Round definition through the console UI (race redesign Slice 2b) — the deliverable proof for the
 * Rounds half of the "Rounds & Heats" stage.
 *
 * A real click-through against a **real** Director (open / no token, full-trust by default): enter
 * an event (Practice), make sure it has ≥1 eligible class (select the built-in **Open Class**), open
 * the workspace's **Rounds & Heats** tab, then **add** a round (label + eligible class + format +
 * win condition + seeding), confirm it lists and **persists across a reload**, **edit** its label,
 * and **remove** it. Cleans up the class selection at the end.
 *
 * Every step is a real click/input in headless chromium on the real `POST/PUT/DELETE
 * /events/{id}/rounds` + `GET /formats` paths — nothing mocked. Importing `test`/`expect` from
 * `./observability.js` means a failure carries the full-stack dump (browser console, page errors,
 * the Director's server log).
 */
import { expect, test } from './observability.js';

/** Get the shared worker Director into the Practice event's workspace. */
async function enterPractice(page: import('@playwright/test').Page) {
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

async function openTab(page: import('@playwright/test').Page, name: string) {
  await page.getByRole('navigation', { name: 'Screens' }).getByRole('button', { name }).click();
}

test('RD defines a round (class, format, seeding), it persists, then edits and removes it', async ({
  page
}) => {
  const LABEL = `E2E-Round-${Date.now()}`;
  await page.goto('/');
  await enterPractice(page);

  // ── Make sure the event has an eligible class: select the built-in Open Class. ──────────────
  await openTab(page, 'Classes & Roster');
  await expect(page.getByRole('heading', { name: 'Present pilots' })).toBeVisible({
    timeout: 15_000
  });
  const classRow = page
    .getByRole('list', { name: 'Class directory' })
    .getByRole('listitem')
    .filter({ hasText: 'Open Class' });
  const classBox = classRow.getByRole('checkbox', { name: 'Select Open Class' });
  if (!(await classBox.isChecked())) {
    const classesSaved = page.waitForResponse(
      (r) => /\/events\/.+\/classes$/.test(r.url()) && r.request().method() === 'PUT'
    );
    await classBox.check();
    await classesSaved;
  }
  await expect(classBox).toBeChecked();

  // ── Rounds & Heats tab → add a round ────────────────────────────────────────────────────────
  await openTab(page, 'Rounds & Heats');
  await expect(page.getByRole('heading', { name: 'Rounds', exact: true })).toBeVisible({
    timeout: 15_000
  });

  // The add-round form is a modal Dialog (ux(rounds)): the trigger opens it, the fields live inside.
  await page.getByRole('button', { name: '+ Add round' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  const form = dialog.getByRole('form', { name: 'Add round' });
  await expect(form).toBeVisible();
  await form.getByLabel('Label').fill(LABEL);
  await form.getByLabel('Format').selectOption('timed_qual');
  // The eligible class is a single-select dropdown (Rounds form redesign item 6).
  await form.getByLabel('Eligible class').selectOption({ label: 'Open Class' });
  // Best lap is the converged **Best-of-N with N = 1** (9705af5) — it still serialises to `BestLap`
  // on the wire, but the picker no longer carries a separate "Best lap" option.
  await form.getByLabel('Win condition').selectOption('BestOfN');
  await form.getByLabel('Laps', { exact: true }).fill('1');
  // Heat-lifecycle config (Slice 3): a 3:30 staging window + a 1.5–3.5s randomized start hold (the
  // start-procedure delays are entered in seconds — Rounds form redesign item 3).
  await form.getByLabel('Staging minutes').fill('3');
  await form.getByLabel('Staging seconds').fill('30');
  await form.getByLabel('Start min delay seconds').fill('1.5');
  await form.getByLabel('Start max delay seconds').fill('3.5');
  await page.getByRole('button', { name: 'Add round', exact: true }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();
  // The dialog closes on a successful add.
  await expect(dialog).toBeHidden();

  // The new round lists, with its format and the resolved class name.
  const list = page.getByRole('list').filter({ hasText: LABEL });
  const row = list.getByRole('listitem').filter({ hasText: LABEL });
  await expect(row).toBeVisible({ timeout: 15_000 });
  // The friendly format name shows in the list (Rounds form redesign item 1); `timed_qual`'s
  // friendly label is "Time Trials" (#218).
  await expect(row.getByText('Time Trials', { exact: true })).toBeVisible();
  await expect(row.getByText('From roster')).toBeVisible();

  // ── It persisted on the Director: a reload resumes into the event with the round listed ─────
  await page.reload();
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await openTab(page, 'Rounds & Heats');
  const rowAfter = page.getByRole('list').getByRole('listitem').filter({ hasText: LABEL });
  await expect(rowAfter).toBeVisible({ timeout: 15_000 });

  // ── Edit the round's label — and confirm the heat-lifecycle config round-tripped on the Director:
  // the edit form is seeded from the persisted round, so the staging mm:ss + start min/max read back.
  await rowAfter.getByRole('button', { name: 'Edit' }).click();
  // Edit opens the same modal Dialog, seeded from the persisted round.
  await expect(page.getByRole('dialog')).toBeVisible();
  const editForm = page.getByRole('dialog').getByRole('form', { name: 'Edit round' });
  await expect(editForm).toBeVisible();
  await expect(editForm.getByLabel('Staging minutes')).toHaveValue('3');
  await expect(editForm.getByLabel('Staging seconds')).toHaveValue('30');
  await expect(editForm.getByLabel('Start min delay seconds')).toHaveValue('1.5');
  await expect(editForm.getByLabel('Start max delay seconds')).toHaveValue('3.5');
  const newLabel = `${LABEL}-v2`;
  await editForm.getByLabel('Label').fill(newLabel);
  await page.getByRole('button', { name: 'Save round' }).click();
  await expect(
    page.getByRole('list').getByRole('listitem').filter({ hasText: newLabel })
  ).toBeVisible({ timeout: 15_000 });

  // ── Remove the round ────────────────────────────────────────────────────────────────────────
  const editedRow = page.getByRole('list').getByRole('listitem').filter({ hasText: newLabel });
  await editedRow.getByRole('button', { name: 'Remove' }).click();
  await expect(
    page.getByRole('list').getByRole('listitem').filter({ hasText: newLabel })
  ).toHaveCount(0, { timeout: 15_000 });

  // ── Clean up: deselect the class so the shared Director's event goes back to empty. ─────────
  await openTab(page, 'Classes & Roster');
  const cleanupBox = page
    .getByRole('list', { name: 'Class directory' })
    .getByRole('listitem')
    .filter({ hasText: 'Open Class' })
    .getByRole('checkbox', { name: 'Select Open Class' });
  if (await cleanupBox.isChecked()) {
    const classesCleared = page.waitForResponse(
      (r) => /\/events\/.+\/classes$/.test(r.url()) && r.request().method() === 'PUT'
    );
    await cleanupBox.uncheck();
    await classesCleared;
  }
});

/**
 * **Multi-select seeding source rounds** (issue #51) — the deliverable proof for aggregated seeding.
 *
 * Enter Practice, select the Open Class, then add two qualifying rounds and a **Head-to-Head** round
 * seeded `FromRanking`. The seeding control reveals a **checkbox multi-select** of the prior rounds;
 * tick both quals and add the finals round. Assert the persisted round carries `source_rounds` with
 * BOTH qual ids (the server aggregates best-per-pilot across them). Cleans up after itself.
 *
 * NOTE: the seeded round used to be a `single_elim` bracket. The tournament generators were carved
 * out for the primitives-first v0.4 (71d49f2; back with #68–#70), so the server 400s that key. The
 * subject here is the **FromRanking multi-select**, which is format-agnostic — so it is exercised on
 * Head-to-Head, a surviving format that seeds from a prior round's ranking exactly the same way.
 */
test('RD seeds a round from multiple source rounds via the multi-select', async ({
  page,
  director
}) => {
  const base = director.baseUrl;
  const ev = `${base}/events/practice`;
  const json = { headers: { 'Content-Type': 'application/json' } };
  const stamp = Date.now();
  const QUAL_A = `E2E-QualA-${stamp}`;
  const QUAL_B = `E2E-QualB-${stamp}`;
  const FINALS = `E2E-Finals-${stamp}`;
  await page.goto('/');
  await enterPractice(page);

  // Select the Open Class so the rounds have an eligible class.
  await openTab(page, 'Classes & Roster');
  const classBox = page
    .getByRole('list', { name: 'Class directory' })
    .getByRole('listitem')
    .filter({ hasText: 'Open Class' })
    .getByRole('checkbox', { name: 'Select Open Class' });
  if (!(await classBox.isChecked())) {
    const classesSaved = page.waitForResponse(
      (r) => /\/events\/.+\/classes$/.test(r.url()) && r.request().method() === 'PUT'
    );
    await classBox.check();
    await classesSaved;
  }
  await expect(classBox).toBeChecked();

  await openTab(page, 'Rounds & Heats');
  await expect(page.getByRole('heading', { name: 'Rounds', exact: true })).toBeVisible({
    timeout: 15_000
  });

  // Add a qualifying round by label (helper, defined below).
  const addQual = async (label: string) => {
    await page.getByRole('button', { name: '+ Add round' }).click();
    const form = page.getByRole('form', { name: 'Add round' });
    await expect(form).toBeVisible();
    await form.getByLabel('Label').fill(label);
    await form.getByLabel('Format').selectOption('timed_qual');
    await form.getByLabel('Eligible class').selectOption({ label: 'Open Class' });
    // Best lap = the converged Best-of-N with N = 1 (9705af5).
    await form.getByLabel('Win condition').selectOption('BestOfN');
    await form.getByLabel('Laps', { exact: true }).fill('1');
    await page.getByRole('button', { name: 'Add round', exact: true }).click();
    await expect(
      page.getByRole('list').getByRole('listitem').filter({ hasText: label })
    ).toBeVisible({ timeout: 15_000 });
  };
  await addQual(QUAL_A);
  await addQual(QUAL_B);

  // Add a head_to_head round seeded FromRanking from BOTH quals via the checkbox multi-select.
  await page.getByRole('button', { name: '+ Add round' }).click();
  const form = page.getByRole('form', { name: 'Add round' });
  await expect(form).toBeVisible();
  await form.getByLabel('Label').fill(FINALS);
  await form.getByLabel('Format').selectOption('head_to_head');
  await form.getByLabel('Eligible class').selectOption({ label: 'Open Class' });
  await form.getByLabel('Seeding').selectOption('FromRanking');
  // The source-rounds multi-select reveals as a checkbox per prior round (issue #51); tick both.
  await form.getByLabel(`Seed from ${QUAL_A}`).check();
  await form.getByLabel(`Seed from ${QUAL_B}`).check();
  await form.getByLabel('Top N').fill('2');
  await page.getByRole('button', { name: 'Add round', exact: true }).click();
  await expect(
    page.getByRole('list').getByRole('listitem').filter({ hasText: FINALS })
  ).toBeVisible({ timeout: 15_000 });

  // It persisted on the Director with `source_rounds` carrying BOTH qual ids.
  const practiceRounds = async () => {
    const events = (await (await page.request.get(`${base}/events`)).json()) as Array<{
      id: string;
      rounds?: Array<{ id: string; label: string; seeding: unknown }>;
    }>;
    return events.find((e) => e.id === 'practice')?.rounds ?? [];
  };
  const rounds = await practiceRounds();
  const qualAId = rounds.find((r) => r.label === QUAL_A)?.id;
  const qualBId = rounds.find((r) => r.label === QUAL_B)?.id;
  const finals = rounds.find((r) => r.label === FINALS);
  expect(qualAId).toBeTruthy();
  expect(qualBId).toBeTruthy();
  const seeding = finals?.seeding as { FromRanking?: { source_rounds: string[]; top_n: number } };
  expect(seeding?.FromRanking?.top_n).toBe(2);
  expect(new Set(seeding?.FromRanking?.source_rounds)).toEqual(new Set([qualAId, qualBId]));

  // ── Clean up: remove the three rounds + deselect the class. ─────────────────────────────────
  for (const label of [FINALS, QUAL_A, QUAL_B]) {
    const id = (await practiceRounds()).find((r) => r.label === label)?.id;
    if (id) await page.request.delete(`${ev}/rounds/${id}`);
  }
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
});

/**
 * **Guided round params + channel-mode toggle** (race redesign Slice 7b) — the deliverable proof.
 *
 * Enter Practice, select the Open Class, open Rounds & Heats and add a `timed_qual` round driving
 * the *guided* params editor: the format's params show inline as labeled fields. For a qualifying
 * format the only param is `rounds`, labeled **"Heats per pilot"** (the qualifying metric is derived
 * from the win condition, so there is no separate `metric` field). Set it, pick the **Static**
 * channel mode, and add the round. Assert it persisted on the Director with the chosen
 * `params.rounds` + `channel_mode: "Static"`. Cleans up after itself.
 */
test('RD adds a round with a guided param and a Static channel mode', async ({
  page,
  director
}) => {
  const base = director.baseUrl;
  const ev = `${base}/events/practice`;
  const json = { headers: { 'Content-Type': 'application/json' } };
  const LABEL = `E2E-Guided-${Date.now()}`;
  await page.goto('/');
  await enterPractice(page);

  // Select the Open Class so a round has an eligible class.
  await openTab(page, 'Classes & Roster');
  const classBox = page
    .getByRole('list', { name: 'Class directory' })
    .getByRole('listitem')
    .filter({ hasText: 'Open Class' })
    .getByRole('checkbox', { name: 'Select Open Class' });
  if (!(await classBox.isChecked())) {
    const classesSaved = page.waitForResponse(
      (r) => /\/events\/.+\/classes$/.test(r.url()) && r.request().method() === 'PUT'
    );
    await classBox.check();
    await classesSaved;
  }
  await expect(classBox).toBeChecked();

  // Rounds & Heats → add a timed_qual round with a guided param + Static channel mode.
  await openTab(page, 'Rounds & Heats');
  await page.getByRole('button', { name: '+ Add round' }).click();
  const form = page.getByRole('form', { name: 'Add round' });
  await expect(form).toBeVisible();
  await form.getByLabel('Label').fill(LABEL);
  await form.getByLabel('Format').selectOption('timed_qual');
  await form.getByLabel('Eligible class').selectOption({ label: 'Open Class' });

  // The channel-mode toggle defaults from the format; force Static.
  await form.getByLabel('Channel mode').selectOption('Static');

  // The format's params show inline as labeled fields (Rounds form redesign item 4); the `rounds`
  // param is labeled "Heats per pilot" (it no longer clashes with the Round entity) and is seeded
  // from its schema default — set it to 5.
  const roundsInput = form.getByLabel('Heats per pilot value');
  await expect(roundsInput).toHaveValue('3'); // seeded from the schema default
  await roundsInput.fill('5');
  if (process.env.GRIDFPV_SHOTS)
    await form.screenshot({ path: `${process.env.GRIDFPV_SHOTS}/guided-params.png` });

  await page.getByRole('button', { name: 'Add round', exact: true }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();
  await expect(page.getByRole('list').getByRole('listitem').filter({ hasText: LABEL })).toBeVisible(
    { timeout: 15_000 }
  );

  // It persisted on the Director with the guided param + Static channel mode.
  const practiceRound = async () => {
    const events = (await (await page.request.get(`${base}/events`)).json()) as Array<{
      id: string;
      rounds?: Array<{ label: string; params: Record<string, string>; channel_mode?: string }>;
    }>;
    return events.find((e) => e.id === 'practice')?.rounds?.find((r) => r.label === LABEL);
  };
  await expect
    .poll(async () => (await practiceRound())?.params?.rounds, { timeout: 15_000 })
    .toBe('5');
  expect((await practiceRound())?.channel_mode).toBe('Static');

  // ── Clean up: remove the round + deselect the class. ──────────────────────────────────────────
  const created = await practiceRound();
  const rounds = (await (await page.request.get(`${base}/events`)).json()) as Array<{
    id: string;
    rounds?: Array<{ id: string; label: string }>;
  }>;
  const roundId = rounds
    .find((e) => e.id === 'practice')
    ?.rounds?.find((r) => r.label === LABEL)?.id;
  expect(created).toBeTruthy();
  if (roundId) await page.request.delete(`${ev}/rounds/${roundId}`);
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
});

/**
 * The **Heats** half of the stage (race redesign Slice 3b) — the deliverable proof for the Heats UI.
 *
 * The prerequisites (a class with two members, a round) are set up over the real REST/control path
 * (the same writes the Roster/Classes/Rounds stages emit), then the test drives the new UI: it
 * clicks **Fill next heat** and asserts a heat appears in that round's list with the resolved
 * pilot callsigns + the round tag, and then **builds a heat by hand** from the round's eligible
 * members and asserts it lists too. Nothing about the Heats UI is mocked.
 */
test('RD fills a round and builds a heat by hand in the Heats UI', async ({ page, director }) => {
  const base = director.baseUrl;
  const ev = `${base}/events/practice`;
  const json = { headers: { 'Content-Type': 'application/json' } };
  const SUFFIX = Date.now();
  const ACE = `E2E-Heat-Ace-${SUFFIX}`;
  const BEE = `E2E-Heat-Bee-${SUFFIX}`;
  const ROUND_LABEL = `E2E-HeatRound-${SUFFIX}`;

  // ── Set up over the real write paths: the Open Class selected, two rostered members, a round ──
  const classes = (await (await page.request.get(`${base}/classes`)).json()) as Array<{
    id: string;
    name: string;
  }>;
  const openClass = classes.find((c) => c.name === 'Open Class');
  expect(openClass, 'the built-in Open Class exists').toBeTruthy();
  const classId = openClass!.id;

  const mkPilot = async (callsign: string) => {
    const p = (await (
      await page.request.post(`${base}/pilots`, { ...json, data: { callsign } })
    ).json()) as { id: string };
    return p.id;
  };
  const aceId = await mkPilot(ACE);
  const beeId = await mkPilot(BEE);

  // Select the class for the event, roster both pilots, and make them members of the class.
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [classId] } });
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [aceId, beeId] } });
  await page.request.put(`${ev}/classes/${classId}/membership`, {
    ...json,
    data: { pilots: [aceId, beeId] }
  });
  // Add a single-class round (FromRoster) so FillRound can draw the two members.
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
        channel_mode: 'PerHeat'
      }
    })
  ).json()) as { id: string };
  expect(round.id).toBeTruthy();

  // ── Into the Heats UI ────────────────────────────────────────────────────────────────────────
  await page.goto('/');
  await enterPractice(page);
  await openTab(page, 'Rounds & Heats');
  const heatRound = page.getByRole('region', { name: `Heats for ${ROUND_LABEL}` });
  await expect(heatRound).toBeVisible({ timeout: 15_000 });

  // ── Generate heats → a heat appears in the round's list with the members' callsigns ───────────
  // timed_qual is deterministic, so the control is "Generate heats" (#216); a 2-pilot whole-field
  // round draws a single heat.
  await heatRound.getByRole('button', { name: 'Generate heats' }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();
  const filledRow = heatRound.locator('.heat-row').first();
  await expect(filledRow).toBeVisible({ timeout: 15_000 });
  await expect(filledRow.getByText(ACE)).toBeVisible();
  await expect(filledRow.getByText(BEE)).toBeVisible();

  // ── Build a heat by hand from the round's eligible members ────────────────────────────────────
  // No id is typed — the heat id is auto-generated (round-scoped + collision-safe); the RD only
  // picks a round and a lineup.
  // The build-heat form is a modal Dialog (ux(rounds)): open it, then fill from inside.
  await page.getByRole('button', { name: '+ Build heat' }).click();
  const buildDialog = page.getByRole('dialog');
  await expect(buildDialog).toBeVisible();
  const buildForm = buildDialog.getByRole('form', { name: 'Build heat' });
  await expect(buildForm).toBeVisible();
  await buildForm.getByLabel('Build round').selectOption({ label: ROUND_LABEL });
  await buildForm.getByLabel(`Select ${ACE}`).check();
  await buildForm.getByLabel(`Select ${BEE}`).check();
  await page.getByRole('button', { name: 'Schedule heat' }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();
  // The dialog closes on a successful schedule.
  await expect(buildDialog).toBeHidden();

  // The hand-built heat now lists under the round too — named by its position in the round (the
  // filled heat is "<round> Heat 1", this hand-built one is "<round> Heat 2"), not its raw id.
  await expect(heatRound.getByText(`${ROUND_LABEL} Heat 2`)).toBeVisible({ timeout: 15_000 });

  // ── Clean up the shared Director's event back to empty. ───────────────────────────────────────
  await page.request.delete(`${ev}/rounds/${round.id}`);
  await page.request.put(`${ev}/classes/${classId}/membership`, {
    ...json,
    data: { pilots: [] }
  });
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [] } });
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
});

/**
 * **Generate heats** — fill a whole deterministic round in one action (#216).
 *
 * Set up a class with **four** members and a `head_to_head` round (`group_size: 2`) over the real
 * write paths — a deterministic format whose field partitions into **two** heats. Then in the Heats
 * UI click the single **Generate heats** control and assert **both** heats appear from that one
 * action (where the old single-step would have drawn only the first). Nothing mocked.
 *
 * NOTE: this used to use `round_robin` (`heat_size: 2`) for the same 4-into-2 partition. That
 * generator was carved out for the primitives-first v0.4 (71d49f2; back with #68–#70) and the server
 * now 400s the key. Head-to-Head with `group_size: 2` gives the identical deterministic 4 → 2 split,
 * which is all this spec needs — the subject is fill-all, not the structure.
 */
test('RD generates a whole round of heats in one action (Generate heats, #216)', async ({
  page,
  director
}) => {
  const base = director.baseUrl;
  const ev = `${base}/events/practice`;
  const json = { headers: { 'Content-Type': 'application/json' } };
  const SUFFIX = Date.now();
  const ROUND_LABEL = `E2E-GenAll-${SUFFIX}`;
  const CALLS = ['A', 'B', 'C', 'D'].map((c) => `E2E-Gen-${c}-${SUFFIX}`);

  // ── Set up over the real write paths: the Open Class, four rostered members, a head_to_head round.
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
  // A head_to_head round, group_size 2 / rotations 1 → 4 members partition into 2 heats, one pass.
  const round = (await (
    await page.request.post(`${ev}/rounds`, {
      ...json,
      data: {
        label: ROUND_LABEL,
        classes: [classId],
        format: 'head_to_head',
        params: { group_size: '2', rotations: '1' },
        win_condition: 'BestLap',
        seeding: 'FromRoster',
        channel_mode: 'PerHeat'
      }
    })
  ).json()) as { id: string };
  expect(round.id).toBeTruthy();

  // ── Into the Heats UI → one "Generate heats" click fills the whole round ──────────────────────
  await page.goto('/');
  await enterPractice(page);
  await openTab(page, 'Rounds & Heats');
  const heatRound = page.getByRole('region', { name: `Heats for ${ROUND_LABEL}` });
  await expect(heatRound).toBeVisible({ timeout: 15_000 });

  await heatRound.getByRole('button', { name: 'Generate heats' }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();

  // Both heats appear from the single action — "<round> Heat 1" AND "<round> Heat 2".
  await expect(heatRound.getByText(`${ROUND_LABEL} Heat 1`)).toBeVisible({ timeout: 15_000 });
  await expect(heatRound.getByText(`${ROUND_LABEL} Heat 2`)).toBeVisible({ timeout: 15_000 });
  await expect(heatRound.locator('.heat-row')).toHaveCount(2);
  if (process.env.GRIDFPV_SHOTS)
    await heatRound.screenshot({ path: `${process.env.GRIDFPV_SHOTS}/generate-all-heats.png` });

  // ── Clean up the shared Director's event back to empty. ───────────────────────────────────────
  await page.request.delete(`${ev}/rounds/${round.id}`);
  await page.request.put(`${ev}/classes/${classId}/membership`, { ...json, data: { pilots: [] } });
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [] } });
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
});

/**
 * The **Channels** UI (race redesign Slice 4b) — the deliverable proof for the channel config +
 * per-heat channel labels.
 *
 * A real click-through: open the **Timers** page, edit the built-in **Mock** timer's channel config
 * (Flexible: pick a couple of catalog channels + add a **custom raw-MHz** entry, set the node
 * count), and save. Then set up a class + two members + a round over the real write paths, fill a
 * heat, and assert each pilot's lineup row shows a resolved **channel label** (a band+channel from
 * the catalog) — the assignment the primary (Mock) timer's available channels drive. Nothing mocked.
 */
test('RD configures a timer’s channels and a filled heat shows channel labels', async ({
  page,
  director
}) => {
  const base = director.baseUrl;
  const ev = `${base}/events/practice`;
  const json = { headers: { 'Content-Type': 'application/json' } };
  const SUFFIX = Date.now();
  const ACE = `E2E-Chan-Ace-${SUFFIX}`;
  const BEE = `E2E-Chan-Bee-${SUFFIX}`;
  const ROUND_LABEL = `E2E-ChanRound-${SUFFIX}`;

  await page.goto('/');
  await enterPractice(page);

  // ── Timers tab → edit the built-in Mock's channel config (Flexible + custom MHz + node count) ──
  await openTab(page, 'Timers');
  const mockRow = page
    .getByRole('list', { name: 'Configured timers' })
    .getByRole('listitem')
    .filter({ hasText: 'Mock' })
    .first();
  await expect(mockRow).toBeVisible({ timeout: 15_000 });
  await mockRow.getByRole('button', { name: 'Edit' }).click();

  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  // Flexible capability, a 4-node cap, two catalog channels + one custom raw-MHz entry.
  await dialog.getByLabel('Channel capability').selectOption('Flexible');
  await dialog.getByLabel('Node count').fill('4');
  await dialog.getByLabel('Raceband R1, 5658 MHz').check();
  await dialog.getByLabel('Raceband R2, 5695 MHz').check();
  // A truly non-catalog frequency, so it lands as a custom chip (not a catalog checkbox).
  const custom = dialog.getByLabel('Custom channel MHz');
  await custom.fill('5670');
  await dialog.getByRole('button', { name: 'Add' }).click();
  await expect(dialog.getByText('5670 MHz')).toBeVisible();
  if (process.env.GRIDFPV_SHOTS)
    await dialog.screenshot({ path: `${process.env.GRIDFPV_SHOTS}/timer-channel-config.png` });
  await dialog.getByRole('button', { name: 'Save changes' }).click();
  await expect(page.getByRole('dialog')).toBeHidden({ timeout: 15_000 });

  // ── Set up a class + two members + a round over the real write paths ──────────────────────────
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
  await page.request.put(`${ev}/classes/${classId}/membership`, {
    ...json,
    data: { pilots: [aceId, beeId] }
  });
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
        channel_mode: 'PerHeat'
      }
    })
  ).json()) as { id: string };

  // ── Into the Heats UI → fill a heat and assert each pilot shows a resolved channel label ───────
  await page.goto('/');
  await enterPractice(page);
  await openTab(page, 'Rounds & Heats');
  const heatRound = page.getByRole('region', { name: `Heats for ${ROUND_LABEL}` });
  await expect(heatRound).toBeVisible({ timeout: 15_000 });
  await heatRound.getByRole('button', { name: 'Generate heats' }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();
  const filledRow = heatRound.locator('.heat-row').first();
  await expect(filledRow).toBeVisible({ timeout: 15_000 });
  // The two pilots are assigned the first two available channels (Raceband R1, R2) and the lineup
  // shows their band+channel labels — the per-heat channel display resolving the raw MHz.
  await expect(filledRow.locator('.lineup-chan').filter({ hasText: 'Raceband R1' })).toBeVisible();
  await expect(filledRow.locator('.lineup-chan').filter({ hasText: 'Raceband R2' })).toBeVisible();
  if (process.env.GRIDFPV_SHOTS)
    await heatRound.screenshot({ path: `${process.env.GRIDFPV_SHOTS}/heat-channel-labels.png` });

  // ── Clean up: round, membership, roster, class selection, and reset the Mock's channels. ──────
  await page.request.delete(`${ev}/rounds/${round.id}`);
  await page.request.put(`${ev}/classes/${classId}/membership`, {
    ...json,
    data: { pilots: [] }
  });
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [] } });
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
  await page.request.put(`${base}/timers/mock`, {
    ...json,
    data: {
      channel_capability: 'Flexible',
      node_count: 8,
      available_channels: [5658, 5695, 5732, 5769, 5806, 5843, 5880, 5917]
    }
  });
});

/**
 * **Results — per-class standings** (race redesign Slice 5/6b) — the deliverable proof.
 *
 * The prerequisites (a class with two members + a `timed_qual` round, its heat filled) are set up
 * over the real REST/control path; then the test drives the **UI**: it runs the filled heat to
 * **Final** (Stage → Start, lets the sim emit laps, ForceEnd → Finalize), opens **Results** and
 * asserts the **per-class standings populate** with the two pilots — the scored result folding all
 * the way through to the class standing. Nothing about the standings UI is mocked.
 *
 * NOTE: this test also used to click **Advance to bracket** and assert a `single_elim` round was
 * created seeded `FromRanking`. Both the button and the bracket generator were carved out for the
 * primitives-first v0.4 (71d49f2) — the advance affordance returns with the tournament builder
 * (#68–#70), and its coverage belongs with it. The standings half is unchanged and still current, so
 * only the advance half was dropped rather than rewriting it onto a format it never described.
 */
test('RD reads per-class standings off a scored round', async ({ page, director }) => {
  const base = director.baseUrl;
  const ev = `${base}/events/practice`;
  const json = { headers: { 'Content-Type': 'application/json' } };
  const SUFFIX = Date.now();
  const ACE = `E2E-Std-Ace-${SUFFIX}`;
  const BEE = `E2E-Std-Bee-${SUFFIX}`;
  const ROUND_LABEL = `E2E-StdRound-${SUFFIX}`;
  const HEAT_ID = `e2e-std-${SUFFIX}`;

  // ── Set up over the real write paths: Open Class selected, two members, a round + a heat ──────
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
  await page.request.put(`${ev}/classes/${classId}/membership`, {
    ...json,
    data: { pilots: [aceId, beeId] }
  });
  const round = (await (
    await page.request.post(`${ev}/rounds`, {
      ...json,
      data: {
        label: ROUND_LABEL,
        classes: [classId],
        format: 'timed_qual',
        params: { rounds: '1' },
        win_condition: 'BestLap',
        seeding: 'FromRoster',
        channel_mode: 'PerHeat'
      }
    })
  ).json()) as { id: string };
  // Schedule the round's heat tagged with the round + class so it lands in the round's list.
  const sched = await page.request.post(`${ev}/control`, {
    ...json,
    data: {
      ScheduleHeat: { heat: HEAT_ID, lineup: [aceId, beeId], class: classId, round: round.id }
    }
  });
  expect(sched.ok()).toBeTruthy();
  // Explicitly focus THIS test's heat as the current heat (SetCurrentHeat) over the control path.
  // The shared worker Director's `current_heat` is pinned to whatever heat the LAST transition or
  // selection touched (it never resets between specs), and filling a heat deliberately does NOT
  // steal Live focus (see current-heat.spec.ts) — so without this explicit selection the run flakes
  // against a stale current heat from an earlier spec. Doing it on the server (not via the Live
  // picker, whose options only re-fetch on a live change) makes this independent of UI timing.
  const focused = await page.request.post(`${ev}/control`, {
    ...json,
    data: { SetCurrentHeat: { heat: HEAT_ID } }
  });
  expect(focused.ok()).toBeTruthy();

  // ── Drive the heat to Final through Live control ─────────────────────────────────────────────
  await page.goto('/');
  await enterPractice(page);
  await expect(page.locator('.conn-label')).toHaveText('live', { timeout: 15_000 });
  // The scheduled heat is now the current heat; run it. The current-heat title renders the friendly
  // "<Round> Heat N" name (the heat is round-tagged), not the raw heat id.
  await expect(page.locator('.heat-id .value')).toHaveText(`${ROUND_LABEL} Heat 1`, {
    timeout: 15_000
  });
  // Heat-lifecycle Slices 1–3: Stage → Start (arms; the runtime auto-advances to Running after a
  // randomized hold). No manual Arm/Start→Running button; ForceEnd is the end-window override.
  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Staged');
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Running', { timeout: 15_000 });

  // Let the sim bank some laps before closing the heat, so the scored result is real.
  const heatSheet = page.getByRole('region', { name: 'Heat sheet' });
  const lapCells = heatSheet.locator('.laps');
  const totalLaps = async () => {
    const texts = await lapCells.allTextContents();
    return texts.reduce((sum, t) => sum + (parseInt(t.match(/(\d+)/)?.[1] ?? '0', 10) || 0), 0);
  };
  await expect.poll(totalLaps, { timeout: 30_000 }).toBeGreaterThan(1);

  await page.getByRole('button', { name: 'ForceEnd', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Unofficial', { timeout: 15_000 });
  await page.getByRole('button', { name: 'Finalize', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Final');

  // ── Results → the per-class standings populate with both pilots ───────────────────────────────
  await openTab(page, 'Results');
  const standings = page.getByRole('table', { name: /Open Class standings/i });
  await expect(standings).toBeVisible({ timeout: 15_000 });
  await expect(standings.getByText(ACE)).toBeVisible();
  await expect(standings.getByText(BEE)).toBeVisible();
  if (process.env.GRIDFPV_SHOTS)
    await page
      .getByRole('region', { name: 'Results' })
      .screenshot({ path: `${process.env.GRIDFPV_SHOTS}/class-standings.png` });

  // ── Rounds & Heats → the scored round's own heat still lists under it (the round survived the
  //    score + fold, and its heat resolves to the friendly "<Round> Heat N" name). ───────────────
  await openTab(page, 'Rounds & Heats');
  const heatRound = page.getByRole('region', { name: `Heats for ${ROUND_LABEL}` });
  await expect(heatRound).toBeVisible({ timeout: 15_000 });
  await expect(heatRound.getByText(`${ROUND_LABEL} Heat 1`)).toBeVisible({ timeout: 15_000 });

  // ── Clean up the shared Director's event back to empty. ───────────────────────────────────────
  const practiceRounds = async (): Promise<Array<{ id: string }>> => {
    const events = (await (await page.request.get(`${base}/events`)).json()) as Array<{
      id: string;
      rounds?: Array<{ id: string }>;
    }>;
    return events.find((e) => e.id === 'practice')?.rounds ?? [];
  };
  for (const r of await practiceRounds()) await page.request.delete(`${ev}/rounds/${r.id}`);
  await page.request.put(`${ev}/classes/${classId}/membership`, {
    ...json,
    data: { pilots: [] }
  });
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [] } });
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
});
