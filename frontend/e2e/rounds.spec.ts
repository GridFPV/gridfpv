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
  await openTab(page, 'Classes');
  await expect(page.getByRole('heading', { name: 'Classes for this event' })).toBeVisible({
    timeout: 15_000
  });
  const classRow = page
    .getByRole('list', { name: 'Class directory' })
    .getByRole('listitem')
    .filter({ hasText: 'Open Class' });
  const classBox = classRow.getByRole('checkbox', { name: 'Select Open Class' });
  if (!(await classBox.isChecked())) await classBox.check();
  await page.getByRole('button', { name: 'Save classes' }).click();
  await expect(page.getByRole('button', { name: 'Save classes' })).toBeDisabled({
    timeout: 15_000
  });

  // ── Rounds & Heats tab → add a round ────────────────────────────────────────────────────────
  await openTab(page, 'Rounds & Heats');
  await expect(page.getByRole('heading', { name: 'Rounds', exact: true })).toBeVisible({
    timeout: 15_000
  });

  await page.getByRole('button', { name: '+ Add round' }).click();
  const form = page.getByRole('form', { name: 'Add round' });
  await expect(form).toBeVisible();
  await form.getByLabel('Label').fill(LABEL);
  await form.getByLabel('Eligible Open Class').check();
  await form.getByLabel('Format').selectOption('timed_qual');
  await form.getByLabel('Win condition').selectOption('BestLap');
  await page.getByRole('button', { name: 'Add round', exact: true }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();

  // The new round lists, with its format and the resolved class name.
  const list = page.getByRole('list').filter({ hasText: LABEL });
  const row = list.getByRole('listitem').filter({ hasText: LABEL });
  await expect(row).toBeVisible({ timeout: 15_000 });
  await expect(row.getByText('timed_qual')).toBeVisible();
  await expect(row.getByText('From roster')).toBeVisible();

  // ── It persisted on the Director: a reload resumes into the event with the round listed ─────
  await page.reload();
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await openTab(page, 'Rounds & Heats');
  const rowAfter = page.getByRole('list').getByRole('listitem').filter({ hasText: LABEL });
  await expect(rowAfter).toBeVisible({ timeout: 15_000 });

  // ── Edit the round's label ──────────────────────────────────────────────────────────────────
  await rowAfter.getByRole('button', { name: 'Edit' }).click();
  const editForm = page.getByRole('form', { name: 'Edit round' });
  await expect(editForm).toBeVisible();
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
  await openTab(page, 'Classes');
  const cleanupBox = page
    .getByRole('list', { name: 'Class directory' })
    .getByRole('listitem')
    .filter({ hasText: 'Open Class' })
    .getByRole('checkbox', { name: 'Select Open Class' });
  if (await cleanupBox.isChecked()) {
    await cleanupBox.uncheck();
    await page.getByRole('button', { name: 'Save classes' }).click();
    await expect(page.getByRole('button', { name: 'Save classes' })).toBeDisabled({
      timeout: 15_000
    });
  }
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
    data: { pilot_ids: [aceId, beeId] }
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
        seeding: 'FromRoster'
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

  // ── Fill next heat → a heat appears in the round's list with the members' callsigns ───────────
  await heatRound.getByRole('button', { name: 'Fill next heat' }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();
  const filledRow = heatRound.locator('.heat-row').first();
  await expect(filledRow).toBeVisible({ timeout: 15_000 });
  await expect(filledRow.getByText(ACE)).toBeVisible();
  await expect(filledRow.getByText(BEE)).toBeVisible();

  // ── Build a heat by hand from the round's eligible members ────────────────────────────────────
  const HAND_HEAT = `e2e-hand-${SUFFIX}`;
  await page.getByRole('button', { name: '+ Build heat' }).click();
  const buildForm = page.getByRole('form', { name: 'Build heat' });
  await expect(buildForm).toBeVisible();
  await buildForm.getByLabel('Build round').selectOption({ label: ROUND_LABEL });
  await buildForm.getByLabel('Build heat id').fill(HAND_HEAT);
  await buildForm.getByLabel(`Select ${ACE}`).check();
  await buildForm.getByLabel(`Select ${BEE}`).check();
  await page.getByRole('button', { name: 'Schedule heat' }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();

  // The hand-built heat now lists under the round too.
  await expect(heatRound.getByText(HAND_HEAT)).toBeVisible({ timeout: 15_000 });

  // ── Clean up the shared Director's event back to empty. ───────────────────────────────────────
  await page.request.delete(`${ev}/rounds/${round.id}`);
  await page.request.put(`${ev}/classes/${classId}/membership`, {
    ...json,
    data: { pilot_ids: [] }
  });
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [] } });
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
});
