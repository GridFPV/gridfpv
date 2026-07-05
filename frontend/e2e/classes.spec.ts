/**
 * Class registry lifecycle through the console UI (#84) — the deliverable proof for the class
 * registry slice with its locked, fixed-id built-ins.
 *
 * Two real click-throughs against a **real** Director (open / no token, full-trust by default):
 *
 *  1. **Built-ins + Custom CRUD** — open the console, land on the **home hub**, open the **Classes**
 *     page, and confirm the 9 standard **built-in** classes are listed with org source badges and a
 *     built-in/lock indicator, and are **not removable** (no Remove button). Then **add** a class:
 *     the form has no org source picker (new classes are `Custom`), and the created row shows the
 *     `Custom` badge and keeps Edit/Remove; finally **remove** it.
 *  2. **In-event selection of a built-in** — enter an event (Practice), open the workspace's
 *     **Classes** tab, **check a built-in** (`Open Class`) into this event's selection — which
 *     **auto-saves** (no Save button); a reload resumes into the event with the built-in still
 *     selected. Cleans up (uncheck auto-saves).
 *
 * Every step is a real click/input in headless chromium on the real `POST/DELETE /classes` +
 * `PUT /events/{id}/classes` paths — nothing mocked. Importing `test`/`expect` from
 * `./observability.js` means a failure carries the full-stack dump (browser console, page errors,
 * the Director's server log).
 */
import { expect, test } from './observability.js';

test('Classes page lists the locked built-ins (org badges, not removable) and full CRUD on a Custom class', async ({
  page
}) => {
  const NAME = `E2E-Class-${Date.now()}`;
  await page.goto('/');

  // The worker's Director may have an active event left by a prior spec (#90): if we resumed into
  // the workspace, use the breadcrumb Home to get back to the hub.
  const classesCard = page.getByRole('heading', { name: 'Classes' });
  const liveNav = page.getByRole('button', { name: /Live control/ });
  await expect(classesCard.or(liveNav).first()).toBeVisible({ timeout: 15_000 });
  if (await liveNav.isVisible().catch(() => false)) {
    await page
      .getByRole('navigation', { name: 'Breadcrumb' })
      .getByRole('button', { name: 'Home' })
      .click();
  }

  // ── Hub → Classes page ─────────────────────────────────────────────────────────────────
  await page.getByRole('button', { name: /Classes/ }).click();
  await expect(page.getByRole('heading', { name: 'Classes', level: 1 })).toBeVisible({
    timeout: 15_000
  });

  // ── The 9 built-ins are present, org-badged, locked (no Remove) ──────────────────────────
  const list = page.getByRole('list', { name: 'Class directory' });
  const openBuiltin = list.getByRole('listitem').filter({ hasText: 'Open Class' });
  await expect(openBuiltin).toBeVisible({ timeout: 15_000 });
  // It carries its real org as a badge + a built-in/lock indicator…
  await expect(openBuiltin.getByText('MultiGP', { exact: true })).toBeVisible();
  await expect(openBuiltin.getByLabel('Built-in')).toBeVisible();
  // …and is read-only: no Edit / Remove on a built-in row.
  await expect(openBuiltin.getByRole('button', { name: 'Remove' })).toHaveCount(0);
  await expect(openBuiltin.getByRole('button', { name: 'Edit' })).toHaveCount(0);
  // A couple more of the 9 are listed, across orgs.
  await expect(list.getByRole('listitem').filter({ hasText: 'Tiny Trainer' })).toBeVisible();
  await expect(list.getByRole('listitem').filter({ hasText: 'Street League' })).toBeVisible();

  // ── Add a class — the form offers NO org source picker; new classes are Custom ───────────
  await page.getByRole('button', { name: '+ Add class' }).click();
  const addForm = page.getByRole('form', { name: 'Add class' });
  await expect(addForm).toBeVisible();
  await expect(addForm.getByLabel('Source')).toHaveCount(0);
  await expect(addForm.getByLabel('Add from MultiGP')).toHaveCount(0);
  await addForm.getByLabel('Name').fill(NAME);

  await page.getByRole('button', { name: 'Add class', exact: true }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();

  // The created class appears as a Custom row with full Edit/Remove controls.
  const row = list.getByRole('listitem').filter({ hasText: NAME });
  await expect(row).toBeVisible({ timeout: 15_000 });
  await expect(row.getByText('Custom', { exact: true })).toBeVisible();
  await expect(row.getByLabel('Built-in')).toHaveCount(0);
  await expect(row.getByRole('button', { name: 'Edit' })).toBeVisible();

  // ── Remove the Custom class (with the confirm step) ──────────────────────────────────────
  await row.getByRole('button', { name: 'Remove' }).click();
  const confirm = page.getByRole('dialog').filter({ hasText: 'Remove class' });
  await expect(confirm).toBeVisible();
  await confirm.getByRole('button', { name: 'Remove' }).click();
  await expect(list.getByRole('listitem').filter({ hasText: NAME })).toHaveCount(0, {
    timeout: 15_000
  });
  // The built-in is still there afterward.
  await expect(list.getByRole('listitem').filter({ hasText: 'Open Class' })).toBeVisible();
});

test('RD selects a built-in class onto the event and it persists', async ({ page }) => {
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

  // ── Open the Classes screen from the workspace sidebar ──────────────────────────────────────
  await page
    .getByRole('navigation', { name: 'Screens' })
    .getByRole('button', { name: 'Classes & Roster' })
    .click();
  await expect(page.getByRole('heading', { name: 'Present pilots' })).toBeVisible({
    timeout: 15_000
  });
  await expect(page.getByText(/selected for this event/i)).toBeVisible();

  // ── A built-in is selectable like any class: checking "Open Class" AUTO-SAVES (no Save button) ─
  const list = page.getByRole('list', { name: 'Class directory' });
  const row = list.getByRole('listitem').filter({ hasText: 'Open Class' });
  await expect(row).toBeVisible({ timeout: 15_000 });
  const box = row.getByRole('checkbox', { name: 'Select Open Class' });
  // Start from a known state (a prior run may have left it checked); ensure checked, waiting for the
  // debounced `PUT …/classes` to land when we actually toggle it on.
  if (!(await box.isChecked())) {
    const classesSaved = page.waitForResponse(
      (r) => /\/events\/.+\/classes$/.test(r.url()) && r.request().method() === 'PUT'
    );
    await box.check();
    await classesSaved;
  }
  await expect(box).toBeChecked();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();

  // ── It persisted on the Director: a reload resumes into the event with the built-in checked ──
  await page.reload();
  await expect(liveNav).toBeVisible({ timeout: 15_000 });
  await page
    .getByRole('navigation', { name: 'Screens' })
    .getByRole('button', { name: 'Classes & Roster' })
    .click();
  const rowAfter = page
    .getByRole('list', { name: 'Class directory' })
    .getByRole('listitem')
    .filter({ hasText: 'Open Class' });
  await expect(rowAfter.getByRole('checkbox', { name: 'Select Open Class' })).toBeChecked({
    timeout: 15_000
  });

  // ── Clean up: uncheck (auto-saves) so the shared Director's event goes back to an empty selection ─
  const boxAfter = rowAfter.getByRole('checkbox', { name: 'Select Open Class' });
  const classesCleared = page.waitForResponse(
    (r) => /\/events\/.+\/classes$/.test(r.url()) && r.request().method() === 'PUT'
  );
  await boxAfter.uncheck();
  await expect(boxAfter).not.toBeChecked();
  await classesCleared;
});

test('RD hides a class on the Classes page; it drops out of the event picker; unhide restores it', async ({
  page
}) => {
  await page.goto('/');

  // ── Get back to the home hub (a prior spec may have left an active event) ────────────────────
  const classesCard = page.getByRole('heading', { name: 'Classes' });
  const liveNav = page.getByRole('button', { name: /Live control/ });
  await expect(classesCard.or(liveNav).first()).toBeVisible({ timeout: 15_000 });
  if (await liveNav.isVisible().catch(() => false)) {
    await page
      .getByRole('navigation', { name: 'Breadcrumb' })
      .getByRole('button', { name: 'Home' })
      .click();
  }

  // ── Open the Classes page and hide a built-in (a visibility preference, allowed for built-ins) ─
  await page.getByRole('button', { name: /Classes/ }).click();
  await expect(page.getByRole('heading', { name: 'Classes', level: 1 })).toBeVisible({
    timeout: 15_000
  });
  const list = page.getByRole('list', { name: 'Class directory' });
  const whoop = list.getByRole('listitem').filter({ hasText: 'Whoop Class' });
  await expect(whoop).toBeVisible({ timeout: 15_000 });

  // Hide it (it stays listed here, marked "Hidden", and now offers "Unhide").
  await whoop.getByRole('button', { name: 'Hide' }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();
  await expect(whoop.getByText('Hidden', { exact: true })).toBeVisible({ timeout: 15_000 });
  await expect(whoop.getByRole('button', { name: 'Unhide' })).toBeVisible();
  // Screenshot: the main view with a class marked Hidden.
  await page.screenshot({ path: 'e2e-artifacts/classes-page-hidden.png', fullPage: true });

  // ── Into the event (Practice) → the Classes & Roster picker must NOT offer the hidden class ──
  await page
    .getByRole('navigation', { name: 'Breadcrumb' })
    .getByRole('button', { name: 'Home' })
    .click();
  const eventsCard = page.getByRole('button', { name: /Events/ });
  await expect(eventsCard).toBeVisible({ timeout: 15_000 });
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
  await page
    .getByRole('navigation', { name: 'Screens' })
    .getByRole('button', { name: 'Classes & Roster' })
    .click();
  await expect(page.getByRole('heading', { name: 'Present pilots' })).toBeVisible({
    timeout: 15_000
  });

  const pickerList = page.getByRole('list', { name: 'Class directory' });
  // The hidden class is filtered out of the per-event picker…
  await expect(pickerList.getByRole('listitem').filter({ hasText: 'Whoop Class' })).toHaveCount(0, {
    timeout: 15_000
  });
  // …while a visible class is still offered.
  await expect(pickerList.getByRole('listitem').filter({ hasText: 'Open Class' })).toBeVisible();
  // Screenshot: the picker with the hidden class excluded.
  await page.screenshot({ path: 'e2e-artifacts/event-picker-hidden-excluded.png', fullPage: true });

  // ── Unhide it back on the Classes page → it returns to the picker (cleanup the shared Director) ─
  await page
    .getByRole('navigation', { name: 'Breadcrumb' })
    .getByRole('button', { name: 'Home' })
    .click();
  await page.getByRole('button', { name: /Classes/ }).click();
  await expect(page.getByRole('heading', { name: 'Classes', level: 1 })).toBeVisible({
    timeout: 15_000
  });
  const whoopAgain = page
    .getByRole('list', { name: 'Class directory' })
    .getByRole('listitem')
    .filter({ hasText: 'Whoop Class' });
  await whoopAgain.getByRole('button', { name: 'Unhide' }).click();
  await expect(whoopAgain.getByText('Hidden', { exact: true })).toHaveCount(0, { timeout: 15_000 });
  await expect(whoopAgain.getByRole('button', { name: 'Hide' })).toBeVisible();
});
