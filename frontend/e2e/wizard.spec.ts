/**
 * The setup wizard (race redesign Slice 7) — the deliverable proof for the guided first-pass over
 * the stage-pages.
 *
 * A real click-through against a **real** Director (open / no token, full-trust by default):
 * **create a new event** with "Set up event" ticked so the wizard opens, then walk its steps —
 * pick a **class** (the built-in Open Class), **add a new pilot** and place them in the class,
 * confirm the **Mock timer** is selected, **define a first round**, reach the **readiness summary**,
 * and **finish**. Then assert the workspace's own stage-pages reflect everything the wizard set
 * (same data, editable on the pages) — proving the wizard is pure orchestration over the existing
 * stage commands, no separate config bag.
 *
 * Every step is a real click/input in headless chromium on the real `PUT /events/{id}/classes`,
 * `POST /pilots` + `PUT roster`/`membership`, `PUT /events/{id}/timers`, `POST /events/{id}/rounds`
 * paths — nothing mocked. Importing `test`/`expect` from `./observability.js` means a failure
 * carries the full-stack dump (browser console, page errors, the Director's server log).
 */
import { expect, test } from './observability.js';

/** Get back to the home hub regardless of any active event a prior spec left (#90). */
async function gotoHub(page: import('@playwright/test').Page) {
  await page.goto('/');
  const eventsCard = page.getByRole('heading', { name: 'Events' });
  const liveNav = page.getByRole('button', { name: /Live control/ });
  await expect(eventsCard.or(liveNav).first()).toBeVisible({ timeout: 15_000 });
  if (await liveNav.isVisible().catch(() => false)) {
    await page
      .getByRole('navigation', { name: 'Breadcrumb' })
      .getByRole('button', { name: 'Home' })
      .click();
    await expect(eventsCard).toBeVisible({ timeout: 15_000 });
  }
}

test('RD creates an event, the wizard walks the stages, and the workspace reflects it all', async ({
  page
}) => {
  const SUFFIX = Date.now();
  const EVENT = `E2E-Wizard-${SUFFIX}`;
  const PILOT = `E2E-WizPilot-${SUFFIX}`;
  const ROUND = `E2E-WizRound-${SUFFIX}`;
  const shots = process.env.GRIDFPV_SHOTS;

  await gotoHub(page);

  // ── Events page → New event with "Set up event" ticked ──────────────────────────────────────
  // With a server-active event from a prior spec, clicking Events may auto-enter that event's
  // workspace (the active event is resolved on load); "Switch event" then reaches the picker.
  await page.getByRole('heading', { name: 'Events' }).click();
  const picker = page.getByRole('heading', { name: 'Choose an event' });
  const switchEvent = page.getByRole('button', { name: /Switch event/ });
  await expect(picker.or(switchEvent).first()).toBeVisible({ timeout: 15_000 });
  if (!(await picker.isVisible().catch(() => false))) {
    await switchEvent.click();
  }
  await expect(picker).toBeVisible({ timeout: 15_000 });
  await page.getByRole('button', { name: '+ New event' }).first().click();
  const newForm = page.getByRole('form', { name: 'New event' });
  await expect(newForm).toBeVisible();
  await newForm.getByLabel('Event name').fill(EVENT);
  // "Set up event" defaults on — confirm it's checked so the wizard launches after create.
  const setupBox = page.getByRole('checkbox', { name: 'Set up event after creating' });
  await expect(setupBox).toBeChecked();
  await page.getByRole('button', { name: 'Create & enter' }).click();

  // ── The wizard overlay opens on its first step (Timer & channels) ───────────────────────────
  const wizard = page.getByRole('dialog', { name: 'Event setup wizard' });
  await expect(wizard).toBeVisible({ timeout: 15_000 });
  await expect(wizard.getByText(`Set up · ${EVENT}`)).toBeVisible();
  await expect(wizard.getByRole('heading', { name: 'Timers for this event' })).toBeVisible({
    timeout: 15_000
  });
  if (shots) await wizard.screenshot({ path: `${shots}/wizard-timers.png` });

  // Step 1 — Timer & channels (now FIRST: per-pilot channels come from the timer). The built-in
  // Mock is selectable; ensure it's chosen (auto-saves, no Save button).
  const mockBox = wizard.getByRole('checkbox', { name: 'Use Mock' });
  await expect(mockBox).toBeVisible({ timeout: 15_000 });
  if (!(await mockBox.isChecked())) {
    const timersSaved = page.waitForResponse(
      (r) => /\/events\/.+\/timers$/.test(r.url()) && r.request().method() === 'PUT'
    );
    await mockBox.check();
    await timersSaved;
  }
  await expect(mockBox).toBeChecked();

  // The ≥1-timer gate: clearing every timer empties the selection (no stale green), disables Next,
  // and shows the hint; re-ticking the Mock clears the row highlight back and re-enables Next.
  const nextBtn = wizard.getByRole('button', { name: 'Next', exact: true });
  await mockBox.uncheck();
  await expect(mockBox).not.toBeChecked();
  await expect(wizard.getByText(/Select at least one timer to continue/i)).toBeVisible();
  await expect(nextBtn).toBeDisabled();
  const timersResaved = page.waitForResponse(
    (r) => /\/events\/.+\/timers$/.test(r.url()) && r.request().method() === 'PUT'
  );
  await mockBox.check();
  await timersResaved;
  await expect(nextBtn).toBeEnabled();

  // Step 2 — Classes & Roster: tick the built-in Open Class — this AUTO-SAVES (no Save button).
  await nextBtn.click();
  const classBox = wizard.getByRole('checkbox', { name: 'Select Open Class' });
  await expect(classBox).toBeVisible({ timeout: 15_000 });
  if (!(await classBox.isChecked())) {
    const classesSaved = page.waitForResponse(
      (r) => /\/events\/.+\/classes$/.test(r.url()) && r.request().method() === 'PUT'
    );
    await classBox.check();
    await classesSaved;
  }
  await expect(classBox).toBeChecked();

  // …then, in the same combined step, add a brand-new pilot and mark present — the roster auto-saves.
  await wizard.getByRole('button', { name: '+ Add pilot' }).click();
  const addForm = page.getByRole('form', { name: 'Add pilot' });
  await expect(addForm).toBeVisible();
  await addForm.getByLabel('Callsign').fill(PILOT);
  // `exact` to pick the dialog's submit, not the header's "+ Add pilot".
  await page.getByRole('button', { name: 'Add pilot', exact: true }).click();
  const rosterBox = wizard.getByRole('checkbox', { name: `Roster ${PILOT}` });
  await expect(rosterBox).toBeVisible({ timeout: 15_000 });
  if (!(await rosterBox.isChecked())) {
    const rosterSaved = page.waitForResponse(
      (r) => /\/events\/.+\/roster$/.test(r.url()) && r.request().method() === 'PUT'
    );
    await rosterBox.check();
    await rosterSaved;
  }
  // With a single class the pilot is auto-placed (no "Place …" checkbox) and the membership
  // auto-saves — the channel selector simply appears, no Save click needed.
  await expect(wizard.getByLabel(`Channel for ${PILOT}`)).toBeVisible({ timeout: 15_000 });

  // Step 3 — First round: the add-round form is pre-opened; define one and add it.
  await wizard.getByRole('button', { name: 'Next', exact: true }).click();
  const roundForm = page.getByRole('form', { name: 'Add round' });
  await expect(roundForm).toBeVisible({ timeout: 15_000 });
  await roundForm.getByLabel('Label').fill(ROUND);
  await roundForm.getByLabel('Format').selectOption('timed_qual');
  await roundForm.getByLabel('Eligible class').selectOption({ label: 'Open Class' });
  await roundForm.getByLabel('Win condition').selectOption('BestLap');
  await page.getByRole('button', { name: 'Add round', exact: true }).click();
  await expect(
    wizard.getByRole('list').getByRole('listitem').filter({ hasText: ROUND })
  ).toBeVisible({ timeout: 15_000 });

  // Step 4 — Review: the readiness summary shows all four checks met, then finish.
  await wizard.getByRole('button', { name: 'Next', exact: true }).click();
  await expect(wizard.getByRole('heading', { name: 'Ready to race?' })).toBeVisible();
  await expect(wizard.getByText('This event is ready to race.')).toBeVisible({ timeout: 15_000 });
  if (shots) await wizard.screenshot({ path: `${shots}/wizard-review.png` });
  await wizard.getByRole('button', { name: 'Finish setup' }).click();
  await expect(wizard).toBeHidden({ timeout: 15_000 });

  // ── The workspace's own stage-pages reflect everything the wizard set (editable on the pages) ──
  const openTab = (name: string) =>
    page.getByRole('navigation', { name: 'Screens' }).getByRole('button', { name }).click();

  // Classes & Roster tab: Open Class is selected, the pilot is present, and (single class) the pilot
  // is auto-placed with its channel selector present.
  await openTab('Classes & Roster');
  await expect(page.getByRole('heading', { name: 'Present pilots' })).toBeVisible({
    timeout: 15_000
  });
  await expect(page.getByRole('checkbox', { name: 'Select Open Class' })).toBeChecked();
  await expect(page.getByRole('checkbox', { name: `Roster ${PILOT}` })).toBeChecked();
  await expect(page.getByLabel(`Channel for ${PILOT}`)).toBeVisible();

  // Rounds & Heats tab: the round defined in the wizard lists.
  await openTab('Rounds & Heats');
  await expect(page.getByRole('list').getByRole('listitem').filter({ hasText: ROUND })).toBeVisible(
    { timeout: 15_000 }
  );

  // ── The wizard is re-runnable from the workspace header ───────────────────────────────────────
  await page.getByRole('button', { name: 'Setup wizard' }).click();
  await expect(page.getByRole('dialog', { name: 'Event setup wizard' })).toBeVisible({
    timeout: 15_000
  });
});
