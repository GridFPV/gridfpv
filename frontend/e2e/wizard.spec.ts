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
  await page.getByRole('heading', { name: 'Events' }).click();
  await expect(page.getByRole('heading', { name: 'Choose an event' })).toBeVisible({
    timeout: 15_000
  });
  await page.getByRole('button', { name: '+ New event' }).first().click();
  const newForm = page.getByRole('form', { name: 'New event' });
  await expect(newForm).toBeVisible();
  await newForm.getByLabel('Event name').fill(EVENT);
  // "Set up event" defaults on — confirm it's checked so the wizard launches after create.
  const setupBox = page.getByRole('checkbox', { name: 'Set up event after creating' });
  await expect(setupBox).toBeChecked();
  await page.getByRole('button', { name: 'Create & enter' }).click();

  // ── The wizard overlay opens on its first step (Classes) ────────────────────────────────────
  const wizard = page.getByRole('dialog', { name: 'Event setup wizard' });
  await expect(wizard).toBeVisible({ timeout: 15_000 });
  await expect(wizard.getByText(`Set up · ${EVENT}`)).toBeVisible();
  if (shots) await wizard.screenshot({ path: `${shots}/wizard-classes.png` });

  // Step 1 — Classes: tick the built-in Open Class and save.
  const classBox = wizard.getByRole('checkbox', { name: 'Select Open Class' });
  await expect(classBox).toBeVisible({ timeout: 15_000 });
  if (!(await classBox.isChecked())) await classBox.check();
  await wizard.getByRole('button', { name: 'Save classes' }).click();
  await expect(wizard.getByRole('button', { name: 'Save classes' })).toBeDisabled({
    timeout: 15_000
  });

  // Step 2 — Roster: add a brand-new pilot, mark present, and place into the class.
  await wizard.getByRole('button', { name: 'Next', exact: true }).click();
  await expect(wizard.getByRole('heading', { name: 'Present pilots' })).toBeVisible();
  await wizard.getByRole('button', { name: '+ Add pilot' }).click();
  const addForm = page.getByRole('form', { name: 'Add pilot' });
  await expect(addForm).toBeVisible();
  await addForm.getByLabel('Callsign').fill(PILOT);
  // `exact` to pick the dialog's submit, not the header's "+ Add pilot".
  await page.getByRole('button', { name: 'Add pilot', exact: true }).click();
  // The new pilot appears; mark them present (roster checkbox), then save the roster.
  const rosterBox = wizard.getByRole('checkbox', { name: `Roster ${PILOT}` });
  await expect(rosterBox).toBeVisible({ timeout: 15_000 });
  if (!(await rosterBox.isChecked())) await rosterBox.check();
  await wizard.getByRole('button', { name: 'Save roster' }).click();
  await expect(wizard.getByRole('button', { name: 'Save roster' })).toBeDisabled({
    timeout: 15_000
  });
  // Place the pilot into the Open Class and save that class's membership.
  const placeBox = wizard.getByRole('checkbox', { name: `Place ${PILOT} in Open Class` });
  await expect(placeBox).toBeVisible({ timeout: 15_000 });
  await placeBox.check();
  await wizard.getByRole('button', { name: 'Save membership' }).click();

  // Step 3 — Timer & channels: the built-in Mock is selectable; ensure it's chosen.
  await wizard.getByRole('button', { name: 'Next', exact: true }).click();
  await expect(wizard.getByRole('heading', { name: 'Timers for this event' })).toBeVisible();
  const mockBox = wizard.getByRole('checkbox', { name: 'Use Mock' });
  await expect(mockBox).toBeVisible({ timeout: 15_000 });
  if (!(await mockBox.isChecked())) {
    await mockBox.check();
    await wizard.getByRole('button', { name: 'Save selection' }).click();
    await expect(wizard.getByRole('button', { name: 'Save selection' })).toBeDisabled({
      timeout: 15_000
    });
  }

  // Step 4 — First round: the add-round form is pre-opened; define one and add it.
  await wizard.getByRole('button', { name: 'Next', exact: true }).click();
  const roundForm = page.getByRole('form', { name: 'Add round' });
  await expect(roundForm).toBeVisible({ timeout: 15_000 });
  await roundForm.getByLabel('Label').fill(ROUND);
  await roundForm.getByLabel('Eligible Open Class').check();
  await roundForm.getByLabel('Format').selectOption('timed_qual');
  await roundForm.getByLabel('Win condition').selectOption('BestLap');
  await page.getByRole('button', { name: 'Add round', exact: true }).click();
  await expect(
    wizard.getByRole('list').getByRole('listitem').filter({ hasText: ROUND })
  ).toBeVisible({ timeout: 15_000 });

  // Step 5 — Review: the readiness summary shows all four checks met, then finish.
  await wizard.getByRole('button', { name: 'Next', exact: true }).click();
  await expect(wizard.getByRole('heading', { name: 'Ready to race?' })).toBeVisible();
  await expect(wizard.getByText('This event is ready to race.')).toBeVisible({ timeout: 15_000 });
  if (shots) await wizard.screenshot({ path: `${shots}/wizard-review.png` });
  await wizard.getByRole('button', { name: 'Finish setup' }).click();
  await expect(wizard).toBeHidden({ timeout: 15_000 });

  // ── The workspace's own stage-pages reflect everything the wizard set (editable on the pages) ──
  const openTab = (name: string) =>
    page.getByRole('navigation', { name: 'Screens' }).getByRole('button', { name }).click();

  // Classes tab: Open Class is selected for the event.
  await openTab('Classes');
  await expect(page.getByRole('heading', { name: 'Classes for this event' })).toBeVisible({
    timeout: 15_000
  });
  await expect(page.getByRole('checkbox', { name: 'Select Open Class' })).toBeChecked();

  // Roster tab: the pilot is present and placed in the class.
  await openTab('Roster');
  await expect(page.getByRole('checkbox', { name: `Roster ${PILOT}` })).toBeChecked();
  await expect(page.getByRole('checkbox', { name: `Place ${PILOT} in Open Class` })).toBeChecked();

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
