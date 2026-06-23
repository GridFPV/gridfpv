/**
 * Rounds form redesign — the deliverable proof for the redesigned create/edit form.
 *
 * A real click-through against a **real** Director (open / no token): enter the Practice event,
 * select the built-in Open Class, open **Rounds & Heats** and open the add-round form. Then drive
 * the redesigned form for two formats and assert the **dynamic per-format field set** + the friendly
 * format names, screenshotting each:
 *
 *  - **Open Practice** — the active-channels picker + time limit show; the eligible-class dropdown,
 *    win condition, seeding and channel mode are hidden.
 *  - **Double Elimination** (a bracket format) — the eligible-class **single-select** dropdown, win
 *    condition, seeding and channel mode show; the active-channels picker is hidden.
 *
 * The format selector shows the **friendly** label ("Open Practice", "Double Elimination") while
 * storing the underlying key. Nothing is mocked. Screenshots land in `e2e/screenshots/` for the PR.
 */
import { expect, test } from './observability.js';
import type { Page } from '@playwright/test';

const SHOTS = new URL('./screenshots/', import.meta.url).pathname;

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

test('the redesigned Rounds form shows the dynamic per-format fields (open practice + a bracket)', async ({
  page,
  director
}) => {
  const base = director.baseUrl;
  const ev = `${base}/events/practice`;
  const json = { headers: { 'Content-Type': 'application/json' } };
  await page.goto('/');
  await enterPractice(page);

  // Select the Open Class so a class round has an eligible class.
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
  await page.getByRole('button', { name: '+ Add round' }).click();
  const form = page.getByRole('form', { name: 'Add round' });
  await expect(form).toBeVisible();
  await form.getByLabel('Label').fill('Form Redesign Demo');

  // ── Open Practice: friendly label, channels picker + time limit, no class / win / seeding ──────
  const formatSelect = form.getByLabel('Format');
  // The selector shows the friendly label while the option value stays the key.
  await expect(formatSelect.locator('option', { hasText: 'Open Practice' })).toHaveCount(1);
  await expect(formatSelect.locator('option', { hasText: 'Double Elimination' })).toHaveCount(1);
  await formatSelect.selectOption('open_practice');
  await expect(form.getByRole('group', { name: 'Active channels' })).toBeVisible();
  await expect(form.getByLabel('Time limit minutes')).toBeVisible();
  await expect(form.getByLabel('Eligible class')).toBeHidden();
  await expect(form.getByLabel('Win condition')).toBeHidden();
  await expect(form.getByLabel('Seeding')).toBeHidden();
  await expect(form.getByLabel('Channel mode')).toBeHidden();
  await form.screenshot({ path: `${SHOTS}rounds-form-open-practice.png` });

  // ── Double Elimination (a bracket): class single-select + win + seeding + channel mode + params ─
  await formatSelect.selectOption('double_elim');
  await expect(form.getByRole('group', { name: 'Active channels' })).toBeHidden();
  // The eligible class is a single-select <select> (Rounds form redesign item 6).
  const classSelect = form.getByLabel('Eligible class');
  await expect(classSelect).toBeVisible();
  await expect(classSelect).toHaveJSProperty('tagName', 'SELECT');
  await classSelect.selectOption({ label: 'Open Class' });
  await expect(form.getByLabel('Win condition')).toBeVisible();
  await expect(form.getByLabel('Seeding')).toBeVisible();
  await expect(form.getByLabel('Channel mode')).toBeVisible();
  // double_elim declares a `bracket_reset` bool param, surfaced inline (item 4).
  await expect(form.getByLabel('Bracket reset value')).toBeVisible();
  // The start-procedure delays are entered in seconds (item 3).
  await expect(form.getByLabel('Start min delay seconds')).toBeVisible();
  // The grace defaults to 30s (item 5).
  await expect(form.getByLabel('Grace window seconds')).toHaveValue('30');
  await form.screenshot({ path: `${SHOTS}rounds-form-bracket.png` });

  // Clean up the shared Director's event back to empty (no round was saved).
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
});

test('the win-condition selector labels the Timed condition "Timed — Most Laps"', async ({
  page,
  director
}) => {
  const base = director.baseUrl;
  const ev = `${base}/events/practice`;
  const json = { headers: { 'Content-Type': 'application/json' } };
  await page.goto('/');
  await enterPractice(page);

  // Select the Open Class so a class round has an eligible class.
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
  await page.getByRole('button', { name: '+ Add round' }).click();
  const form = page.getByRole('form', { name: 'Add round' });
  await expect(form).toBeVisible();

  // A bracket format surfaces the win-condition selector. The Timed condition is a misnomer-free
  // "Timed — Most Laps" (the win is most laps within the set time; the time is just the parameter).
  await form.getByLabel('Format').selectOption('double_elim');
  const winSelect = form.getByLabel('Win condition');
  await expect(winSelect).toBeVisible();
  await expect(winSelect.locator('option', { hasText: 'Timed — Most Laps' })).toHaveCount(1);
  await winSelect.selectOption('Timed');
  // The time parameter reads as the duration parameter, not the win condition itself.
  await expect(form.getByLabel('Race time seconds')).toBeVisible();
  await form.screenshot({ path: `${SHOTS}rounds-form-timed-most-laps.png` });

  // Clean up the shared Director's event back to empty (no round was saved).
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
});

test('a qualifying round: win condition IS the metric, "Heats per pilot", heats named "<Round> Heat N"', async ({
  page,
  director
}) => {
  const base = director.baseUrl;
  const ev = `${base}/events/practice`;
  const json = { headers: { 'Content-Type': 'application/json' } };
  const OPEN_CLASS = 'mgp-open';

  // ── Set up (via the open API): select the Open Class, roster two pilots, make them its members,
  //    and add a Qualifying (timed_qual) round, PerHeat so each format-round is one whole-field heat.
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [OPEN_CLASS] } });
  const mk = async (callsign: string): Promise<string> => {
    const r = await page.request.post(`${base}/pilots`, { ...json, data: { callsign } });
    return (await r.json()).id as string;
  };
  const p1 = await mk('QualAce');
  const p2 = await mk('QualBolt');
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [p1, p2] } });
  await page.request.put(`${ev}/classes/${OPEN_CLASS}/membership`, {
    ...json,
    data: { pilots: [{ pilot: p1 }, { pilot: p2 }] }
  });
  // Two format-rounds so the round produces two heats; PerHeat so each is one whole-field heat.
  const roundRes = await page.request.post(`${ev}/rounds`, {
    ...json,
    data: {
      label: 'Qualifying',
      classes: [OPEN_CLASS],
      format: 'timed_qual',
      params: { rounds: '2' },
      win_condition: 'BestLap',
      channel_mode: 'PerHeat'
    }
  });
  const roundId = (await roundRes.json()).id as string;

  await page.goto('/');
  await enterPractice(page);
  await openTab(page, 'Rounds & Heats');

  // ── The form for the Qualifying (timed_qual) format: the win condition IS the metric ───────────
  await page.getByRole('button', { name: '+ Add round' }).click();
  const form = page.getByRole('form', { name: 'Add round' });
  await expect(form).toBeVisible();
  await form.getByLabel('Format').selectOption('timed_qual');

  // The win-condition selector shows only the qualifying conditions (no First-to-N), and there is
  // NO separate "qualifying metric" field — the win condition drives the qualifying ranking.
  const win = form.getByLabel('Win condition');
  await expect(win).toBeVisible();
  await expect(win.locator('option', { hasText: 'Best lap' })).toHaveCount(1);
  await expect(win.locator('option', { hasText: 'Best N consecutive' })).toHaveCount(1);
  await expect(win.locator('option', { hasText: 'Timed — Most Laps' })).toHaveCount(1);
  await expect(win.locator('option', { hasText: 'First to N laps' })).toHaveCount(0);
  await expect(form.getByLabel('Qualifying metric value')).toBeHidden();
  await expect(form.getByLabel('Ranking metric value')).toBeHidden();
  // The `rounds` param is relabeled "Heats per pilot".
  await expect(form.getByLabel('Heats per pilot value')).toBeVisible();
  await form.screenshot({ path: `${SHOTS}rounds-form-qualifying.png` });
  // Close the add-form without saving (the round was created via the API above).
  await form.getByRole('button', { name: 'Cancel' }).click();

  // ── Two heats in the round → named by position: "Qualifying Heat 1" / "Qualifying Heat 2".
  // Schedule them via the control API tagged with the round (the generator only emits the next heat
  // once the prior is finalized; two ScheduleHeats give the two heats this naming test needs without
  // running them).
  for (const heatId of ['round-1', 'round-2']) {
    await page.request.post(`${ev}/control`, {
      ...json,
      data: {
        ScheduleHeat: { heat: heatId, lineup: [p1, p2], class: OPEN_CLASS, round: roundId }
      }
    });
  }
  // The Heats list re-reads on a live-state push OR on a fresh mount. Filling a heat does NOT steal
  // Live focus (current-heat.spec.ts), so on the shared worker Director — where a prior spec may
  // have left a heat current — scheduling these does not necessarily push a live change, leaving the
  // list stale. Reload to force a fresh fetch so this assertion is order-independent.
  await page.reload();
  await page
    .getByRole('navigation', { name: 'Screens' })
    .getByRole('button', { name: 'Rounds & Heats' })
    .click();
  const heatsRegion = page.getByRole('region', { name: 'Heats for Qualifying' });
  await expect(heatsRegion.getByText('Qualifying Heat 1')).toBeVisible({ timeout: 15_000 });
  await expect(heatsRegion.getByText('Qualifying Heat 2')).toBeVisible({ timeout: 15_000 });
  // The raw generated heat ids are not shown IN THE HEATS LIST — scoped to that region, because the
  // Live HUD's ContextHeader legitimately shows the current heat's raw id (and on the shared worker
  // Director one of these scheduled heats is the current heat), which a page-wide assert would catch.
  await expect(heatsRegion.getByText('round-1', { exact: true })).toHaveCount(0);
  await heatsRegion.screenshot({ path: `${SHOTS}rounds-qualifying-heat-names.png` });

  // Clean up the shared Director's event back to empty.
  await page.request.delete(`${ev}/rounds/${roundId}`);
  await page.request.put(`${ev}/classes/${OPEN_CLASS}/membership`, {
    ...json,
    data: { pilots: [] }
  });
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [] } });
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
});
