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

  // ── Open the Registration screen (the EventRoster) from the workspace sidebar ───────────────
  await page
    .getByRole('navigation', { name: 'Screens' })
    .getByRole('button', { name: 'Registration' })
    .click();
  await expect(page.getByRole('heading', { name: 'Roster for this event' })).toBeVisible({
    timeout: 15_000
  });
  // The roster count header is present.
  await expect(page.getByText(/rostered for this event/i)).toBeVisible();

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
    .getByRole('button', { name: 'Registration' })
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
