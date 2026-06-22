/**
 * In-event roster through the console UI (#74) — the deliverable proof for the event-roster slice.
 *
 * A person enters an event (Practice), opens the workspace's **Registration** screen (now the
 * EventRoster), and — without leaving the event — **registers a brand-new pilot inline**, then
 * **checks it into this event's roster** and **saves**. The roster is asserted to have persisted on
 * the Director (a reload resumes into the event with the pilot still checked). Finally the pilot is
 * **unchecked + saved** (roster shrinks) and **removed** from the directory — cleaning up after
 * itself, since the worker's Director is shared.
 *
 * Every step is a real click/input in headless chromium on the real `POST /pilots` +
 * `PUT /events/{id}/roster` paths — nothing mocked. Importing `test`/`expect` from
 * `./observability.js` means a failure carries the full-stack dump (browser console, page errors,
 * the Director's server log).
 */
import { expect, test } from './observability.js';

const CALLSIGN = `E2E-Roster-${Date.now()}`;

test('RD registers a pilot inline and checks it into the event roster', async ({ page }) => {
  await page.goto('/');

  // ── Get into an event (Practice). The worker's Director may already have an active event from a
  //    prior spec. On a fresh load the hash is authoritative (#118), so we land on the hub even when
  //    an event is active; clicking Events then either opens the picker (no event active) or
  //    auto-enters the active event's workspace. Either way we end up in a workspace on Practice. ──
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

  // ── Open the Roster stage (the EventRoster) from the workspace sidebar ───────────────
  await page
    .getByRole('navigation', { name: 'Screens' })
    .getByRole('button', { name: 'Classes & Roster' })
    .click();
  await expect(page.getByRole('heading', { name: 'Present pilots' })).toBeVisible({
    timeout: 15_000
  });
  // The roster count header is present.
  await expect(page.getByText(/present at this event/i)).toBeVisible();

  // ── Register a brand-new pilot inline — without leaving the event ────────────────────────────
  await page.getByRole('button', { name: '+ Add pilot' }).click();
  const addForm = page.getByRole('form', { name: 'Add pilot' });
  await expect(addForm).toBeVisible();
  await addForm.getByLabel('Callsign').fill(CALLSIGN);
  // Submit (open Director — no token prompt). `exact` to pick the dialog's submit, not the header's.
  await page.getByRole('button', { name: 'Add pilot', exact: true }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();

  // The new pilot appears in the list as a fresh, unchecked, selectable row.
  const list = page.getByRole('list', { name: 'Pilot directory' });
  const row = list.getByRole('listitem').filter({ hasText: CALLSIGN });
  await expect(row).toBeVisible({ timeout: 15_000 });
  const box = row.getByRole('checkbox', { name: `Roster ${CALLSIGN}` });
  await expect(box).not.toBeChecked();

  // ── Check it into the event roster, then Save ───────────────────────────────────────────────
  await box.check();
  await expect(box).toBeChecked();
  await page.getByRole('button', { name: 'Save roster' }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();
  // After a successful save there is nothing pending, so Save goes disabled again.
  await expect(page.getByRole('button', { name: 'Save roster' })).toBeDisabled({ timeout: 15_000 });

  // ── It persisted on the Director: a reload resumes into the event with the pilot still checked.
  await page.reload();
  await expect(liveNav).toBeVisible({ timeout: 15_000 });
  await page
    .getByRole('navigation', { name: 'Screens' })
    .getByRole('button', { name: 'Classes & Roster' })
    .click();
  const rowAfter = page
    .getByRole('list', { name: 'Pilot directory' })
    .getByRole('listitem')
    .filter({ hasText: CALLSIGN });
  await expect(rowAfter.getByRole('checkbox', { name: `Roster ${CALLSIGN}` })).toBeChecked({
    timeout: 15_000
  });

  // ── Uncheck + Save: the roster shrinks back (the remove side of the toggle) ──────────────────
  const boxAfter = rowAfter.getByRole('checkbox', { name: `Roster ${CALLSIGN}` });
  await boxAfter.uncheck();
  await expect(boxAfter).not.toBeChecked();
  await page.getByRole('button', { name: 'Save roster' }).click();
  await expect(page.getByRole('button', { name: 'Save roster' })).toBeDisabled({ timeout: 15_000 });

  // ── Clean up: remove the pilot from the directory (the worker's Director is shared) ──────────
  await rowAfter.getByRole('button', { name: 'Remove' }).click();
  const confirm = page.getByRole('dialog').filter({ hasText: 'Remove pilot' });
  await expect(confirm).toBeVisible();
  await confirm.getByRole('button', { name: 'Remove' }).click();
  await expect(
    page.getByRole('list', { name: 'Pilot directory' }).getByRole('listitem').filter({
      hasText: CALLSIGN
    })
  ).toHaveCount(0, { timeout: 15_000 });
});

/**
 * The combined **Classes & Roster** stage (race redesign Slice 7b): single-class **auto-fill** + the
 * per-pilot **channel** (the static binding), end to end against a real Director.
 *
 * Enter an event (Practice), select the built-in **Open Class** onto it, then in the same combined
 * stage: register a pilot inline and mark them present. Because exactly one class is selected the
 * pilot is **auto-placed** (no per-class checkbox); the RD assigns them a **channel** drawn from the
 * primary timer's available channels and saves the placement — the key proof, which we assert
 * **persists across a reload** (the channel seeds off `EventMeta.classes_membership`'s `MemberSlot`).
 * Cleans up after itself (the worker's Director is shared): unticks the class and removes the pilot.
 */
test('RD auto-fills a single class and assigns a pilot a channel (persists)', async ({ page }) => {
  const CS = `E2E-Member-${Date.now()}`;
  const nav = page.getByRole('navigation', { name: 'Screens' });
  await page.goto('/');

  // ── Get into an event (Practice). Same resume-tolerant dance as the spec above. ──
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

  // ── Select the built-in "Open Class" onto the event (the Classes tab) so the Roster stage has a
  //    class to place pilots into. ──
  await nav.getByRole('button', { name: 'Classes & Roster' }).click();
  await expect(page.getByRole('heading', { name: 'Present pilots' })).toBeVisible({
    timeout: 15_000
  });
  const classRow = page
    .getByRole('list', { name: 'Class directory' })
    .getByRole('listitem')
    .filter({ hasText: 'Open Class' });
  const classBox = classRow.getByRole('checkbox', { name: 'Select Open Class' });
  if (!(await classBox.isChecked())) await classBox.check();
  await expect(classBox).toBeChecked();
  await page.getByRole('button', { name: 'Save classes' }).click();
  await expect(page.getByRole('button', { name: 'Save classes' })).toBeDisabled({
    timeout: 15_000
  });

  // ── Roster stage: register a pilot inline + mark present ──────────────────────────────────────
  await nav.getByRole('button', { name: 'Classes & Roster' }).click();
  await expect(page.getByRole('heading', { name: 'Present pilots' })).toBeVisible({
    timeout: 15_000
  });
  await page.getByRole('button', { name: '+ Add pilot' }).click();
  const addForm = page.getByRole('form', { name: 'Add pilot' });
  await expect(addForm).toBeVisible();
  await addForm.getByLabel('Callsign').fill(CS);
  await page.getByRole('button', { name: 'Add pilot', exact: true }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();

  const dir = page.getByRole('list', { name: 'Pilot directory' });
  const pilotRow = dir.getByRole('listitem').filter({ hasText: CS });
  await expect(pilotRow).toBeVisible({ timeout: 15_000 });
  await pilotRow.getByRole('checkbox', { name: `Roster ${CS}` }).check();
  await page.getByRole('button', { name: 'Save roster' }).click();
  await expect(page.getByRole('button', { name: 'Save roster' })).toBeDisabled({ timeout: 15_000 });

  // ── Single class → auto-fill: the present pilot is automatically placed (no checkbox); assign it
  //    a channel from the primary timer and save the placement. The channel IS the static binding. ──
  const channelSel = page.getByLabel(`Channel for ${CS}`);
  await expect(channelSel).toBeVisible({ timeout: 15_000 });
  // Pick the first real channel option (skip the "No channel" sentinel).
  const firstChannel = await channelSel.locator('option').nth(1).getAttribute('value');
  await channelSel.selectOption(firstChannel!);
  if (process.env.GRIDFPV_SHOTS)
    await page
      .getByRole('region', { name: 'Classes and roster' })
      .screenshot({ path: `${process.env.GRIDFPV_SHOTS}/classes-roster-stage.png` });
  const savePlacement = page.getByRole('button', { name: 'Save placement' });
  await savePlacement.click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();
  await expect(savePlacement).toBeDisabled({ timeout: 15_000 });

  // ── The placement + channel persisted: a reload seeds the channel off
  //    `EventMeta.classes_membership` (the MemberSlot's channel). ──
  await page.reload();
  await expect(liveNav).toBeVisible({ timeout: 15_000 });
  await nav.getByRole('button', { name: 'Classes & Roster' }).click();
  const channelAfter = page.getByLabel(`Channel for ${CS}`);
  await expect(channelAfter).toHaveValue(firstChannel!, { timeout: 15_000 });

  // ── Clean up (shared Director): untick the class (which clears its membership), remove the pilot ─
  await nav.getByRole('button', { name: 'Classes & Roster' }).click();
  const classRowAfter = page
    .getByRole('list', { name: 'Class directory' })
    .getByRole('listitem')
    .filter({ hasText: 'Open Class' });
  const classBoxAfter = classRowAfter.getByRole('checkbox', { name: 'Select Open Class' });
  if (await classBoxAfter.isChecked()) {
    await classBoxAfter.uncheck();
    await page.getByRole('button', { name: 'Save classes' }).click();
    await expect(page.getByRole('button', { name: 'Save classes' })).toBeDisabled({
      timeout: 15_000
    });
  }

  await nav.getByRole('button', { name: 'Classes & Roster' }).click();
  const cleanupRow = page
    .getByRole('list', { name: 'Pilot directory' })
    .getByRole('listitem')
    .filter({ hasText: CS });
  await cleanupRow.getByRole('button', { name: 'Remove' }).click();
  const confirm2 = page.getByRole('dialog').filter({ hasText: 'Remove pilot' });
  await expect(confirm2).toBeVisible();
  await confirm2.getByRole('button', { name: 'Remove' }).click();
  await expect(
    page
      .getByRole('list', { name: 'Pilot directory' })
      .getByRole('listitem')
      .filter({ hasText: CS })
  ).toHaveCount(0, { timeout: 15_000 });
});
