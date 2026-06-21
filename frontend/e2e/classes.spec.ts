/**
 * Class registry lifecycle through the console UI (#84) — the deliverable proof for the class
 * registry slice.
 *
 * Two real click-throughs against a **real** Director (open / no token, full-trust by default):
 *
 *  1. **Directory CRUD** — open the console, land on the **home hub**, open the **Classes** page,
 *     **add** a class via the **MultiGP quick-pick** (which pre-fills `source = MultiGP` + a
 *     reference) and see it listed with its source badge + reference link; **edit** it and **clear**
 *     the reference + description (the clear-via-`null` wiring), confirming they actually clear; then
 *     **remove** it.
 *  2. **In-event selection** — enter an event (Practice), open the workspace's **Classes** tab,
 *     register a class inline, **check it** into this event's selection and **Save**; a reload
 *     resumes into the event with the class still selected. Cleans up after itself (uncheck + remove)
 *     since the worker's Director is shared.
 *
 * Every step is a real click/input in headless chromium on the real `POST/PUT/DELETE /classes` +
 * `PUT /events/{id}/classes` paths — nothing mocked. Importing `test`/`expect` from
 * `./observability.js` means a failure carries the full-stack dump (browser console, page errors,
 * the Director's server log).
 */
import { expect, test } from './observability.js';

test('RD adds (via MultiGP quick-pick), edits (clearing reference + description), and removes a directory class', async ({
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

  // ── Add a class via the MultiGP quick-pick, then override the name to a unique one ───────
  await page.getByRole('button', { name: '+ Add class' }).click();
  const addForm = page.getByRole('form', { name: 'Add class' });
  await expect(addForm).toBeVisible();

  // Pick "Open" from the MultiGP quick-pick: this pre-fills source=MultiGP + a reference URL.
  await addForm.getByLabel('Add from MultiGP').selectOption('Open');
  await expect(addForm.getByLabel('Source')).toHaveValue('MultiGP');
  await expect(addForm.getByLabel('Reference')).toHaveValue(/^https?:\/\//);
  // Use a unique name so the shared Director stays isolated; add a description.
  await addForm.getByLabel('Name').fill(NAME);
  await addForm.getByLabel('Description').fill('A quick-picked MultiGP class.');

  // Submit (open Director — no token prompt). `exact` so it picks the dialog's submit, not the
  // page header's "+ Add class".
  await page.getByRole('button', { name: 'Add class', exact: true }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();

  // It appears in the directory with its name, a MultiGP source badge, and a reference link.
  const list = page.getByRole('list', { name: 'Class directory' });
  const row = list.getByRole('listitem').filter({ hasText: NAME });
  await expect(row).toBeVisible({ timeout: 15_000 });
  await expect(row.getByText('MultiGP', { exact: true })).toBeVisible();
  await expect(row.getByRole('link', { name: `Reference for ${NAME}` })).toBeVisible();
  await expect(row.getByText('A quick-picked MultiGP class.')).toBeVisible();

  // ── Edit: clear the reference + description ───────────────────────────────────────────────
  await row.getByRole('button', { name: 'Edit' }).click();
  const editForm = page.getByRole('form', { name: 'Edit class' });
  await expect(editForm).toBeVisible();
  await editForm.getByLabel('Reference').fill('');
  await editForm.getByLabel('Description').fill('');
  await page.getByRole('button', { name: 'Save changes' }).click();

  // The row no longer renders a reference link or the description — the clear actually persisted
  // (the edit sent `null` for reference/description; re-reading the directory shows them gone).
  await expect(row.getByRole('link', { name: `Reference for ${NAME}` })).toHaveCount(0, {
    timeout: 15_000
  });
  await expect(row.getByText('A quick-picked MultiGP class.')).toHaveCount(0);
  // The name + source badge are unchanged (only reference/description were cleared).
  await expect(row.getByText(NAME)).toBeVisible();
  await expect(row.getByText('MultiGP', { exact: true })).toBeVisible();

  // ── Remove (with the confirm step) ─────────────────────────────────────────────────────
  await row.getByRole('button', { name: 'Remove' }).click();
  const confirm = page.getByRole('dialog').filter({ hasText: 'Remove class' });
  await expect(confirm).toBeVisible();
  await confirm.getByRole('button', { name: 'Remove' }).click();

  // The class is gone from the directory.
  await expect(list.getByRole('listitem').filter({ hasText: NAME })).toHaveCount(0, {
    timeout: 15_000
  });
});

test('RD registers a class inline and checks it into the event class selection', async ({
  page
}) => {
  const NAME = `E2E-EventClass-${Date.now()}`;
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
    .getByRole('button', { name: 'Classes' })
    .click();
  await expect(page.getByRole('heading', { name: 'Classes for this event' })).toBeVisible({
    timeout: 15_000
  });
  // The selection count header is present.
  await expect(page.getByText(/selected for this event/i)).toBeVisible();

  // ── Register a brand-new class inline — without leaving the event ────────────────────────────
  await page.getByRole('button', { name: '+ Add class' }).click();
  const addForm = page.getByRole('form', { name: 'Add class' });
  await expect(addForm).toBeVisible();
  await addForm.getByLabel('Name').fill(NAME);
  await page.getByRole('button', { name: 'Add class', exact: true }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();

  // The new class appears in the list as a fresh, unchecked, selectable row.
  const list = page.getByRole('list', { name: 'Class directory' });
  const row = list.getByRole('listitem').filter({ hasText: NAME });
  await expect(row).toBeVisible({ timeout: 15_000 });
  const box = row.getByRole('checkbox', { name: `Select ${NAME}` });
  await expect(box).not.toBeChecked();

  // ── Check it into the event's class selection, then Save ─────────────────────────────────────
  await box.check();
  await expect(box).toBeChecked();
  await page.getByRole('button', { name: 'Save classes' }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();
  // After a successful save there is nothing pending, so Save goes disabled again.
  await expect(page.getByRole('button', { name: 'Save classes' })).toBeDisabled({
    timeout: 15_000
  });

  // ── It persisted on the Director: a reload resumes into the event with the class still checked.
  await page.reload();
  await expect(liveNav).toBeVisible({ timeout: 15_000 });
  await page
    .getByRole('navigation', { name: 'Screens' })
    .getByRole('button', { name: 'Classes' })
    .click();
  const rowAfter = page
    .getByRole('list', { name: 'Class directory' })
    .getByRole('listitem')
    .filter({ hasText: NAME });
  await expect(rowAfter.getByRole('checkbox', { name: `Select ${NAME}` })).toBeChecked({
    timeout: 15_000
  });

  // ── Uncheck + Save: the selection shrinks back ───────────────────────────────────────────────
  const boxAfter = rowAfter.getByRole('checkbox', { name: `Select ${NAME}` });
  await boxAfter.uncheck();
  await expect(boxAfter).not.toBeChecked();
  await page.getByRole('button', { name: 'Save classes' }).click();
  await expect(page.getByRole('button', { name: 'Save classes' })).toBeDisabled({
    timeout: 15_000
  });

  // ── Clean up: remove the class from the directory (the worker's Director is shared) ──────────
  await rowAfter.getByRole('button', { name: 'Remove' }).click();
  const confirm = page.getByRole('dialog').filter({ hasText: 'Remove class' });
  await expect(confirm).toBeVisible();
  await confirm.getByRole('button', { name: 'Remove' }).click();
  await expect(
    page.getByRole('list', { name: 'Class directory' }).getByRole('listitem').filter({
      hasText: NAME
    })
  ).toHaveCount(0, { timeout: 15_000 });
});
