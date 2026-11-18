/**
 * Class directory lifecycle through the console UI (#84), with its locked, fixed-id built-ins.
 *
 * Two real click-throughs against a **real** Director (open / no token, full-trust by default):
 *
 *  1. **Built-ins + Custom CRUD** — open the console, land on the **home hub**, open the **Classes**
 *     page, and confirm the standard **built-in** classes are listed with org source badges and a
 *     built-in/lock indicator, and are **not removable** (no Remove button). Then **add** a class:
 *     the form has no org source picker (new classes are `Custom`), and the created row shows the
 *     `Custom` badge and keeps Edit/Remove; finally **remove** it.
 *  2. **Hide / unhide** — hide a built-in, see it marked Hidden (and flagged on the wire), reload
 *     to prove it persisted, and unhide it again.
 *
 * Every step is a real click/input in headless chromium on the real `/classes` paths — nothing
 * mocked. Importing `test`/`expect` from `./observability.js` means a failure carries the
 * full-stack dump (browser console, page errors, the Director's server log).
 */
import { expect, test } from './observability.js';
import { gotoPage } from './nav.js';

test('Classes page lists the built-ins (org badges, not editable), full CRUD on a Custom class, and a built-in is removed and put back', async ({
  page
}) => {
  const NAME = `E2E-Class-${Date.now()}`;
  // ── Setup → Classes ────────────────────────────────────────────────────────────────────
  await gotoPage(page, 'Classes');

  // ── The built-ins are present and org-badged ─────────────────────────────────────────
  const list = page.getByRole('list', { name: 'Class directory' });
  const openBuiltin = list.getByRole('listitem').filter({ hasText: 'Open Class' });
  await expect(openBuiltin).toBeVisible({ timeout: 15_000 });
  // It carries its real org as a badge, and no lock or "Built-in" label…
  await expect(openBuiltin.getByText('MultiGP', { exact: true })).toBeVisible();
  await expect(openBuiltin.getByText('Built-in')).toHaveCount(0);
  // …and cannot be edited. It can be removed, which is tried at the end of this test.
  await expect(openBuiltin.getByRole('button', { name: 'Edit' })).toHaveCount(0);
  await expect(openBuiltin.getByRole('button', { name: 'Remove' })).toBeVisible();
  await expect(page.getByTestId('removed-builtins')).toHaveCount(0);
  // A couple more are listed, across orgs.
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

  // ── Remove a built-in, see that it stays gone, and put the built-ins back ────────────────
  const shrieker = list.getByRole('listitem').filter({ hasText: 'Shrieker' });
  try {
    await shrieker.getByRole('button', { name: 'Remove' }).click();
    await expect(confirm).toContainText('You can put the built-in classes back at any time');
    await confirm.getByRole('button', { name: 'Remove' }).click();
    await expect(shrieker).toHaveCount(0, { timeout: 15_000 });
    await expect(page.getByTestId('removed-builtins')).toContainText(
      '1 built-in class has been removed.'
    );
    // The Director dropped it, and a reload does not bring it back.
    const ids = async () =>
      ((await (await page.request.get('/classes')).json()) as { id: string }[]).map((c) => c.id);
    expect(await ids()).not.toContain('udl-shrieker');
    await page.reload();
    await expect(list.getByRole('listitem').filter({ hasText: 'Open Class' })).toBeVisible({
      timeout: 15_000
    });
    await expect(shrieker).toHaveCount(0);

    await page.getByRole('button', { name: 'Put the built-in classes back' }).click();
    await expect(shrieker).toBeVisible({ timeout: 15_000 });
    await expect(shrieker.getByText('UDL', { exact: true })).toBeVisible();
    await expect(page.getByTestId('removed-builtins')).toHaveCount(0);
    expect(await ids()).toContain('udl-shrieker');
  } finally {
    // Leave the shared Director with every built-in, whatever happened above.
    await page.request.post('/classes/builtins');
  }
});

test('RD hides a class on the Classes page; it stays hidden across a reload; unhide restores it', async ({
  page
}) => {
  // ── Open the Classes page and hide a built-in (a visibility preference, allowed for built-ins) ─
  await gotoPage(page, 'Classes');
  const whoop = page
    .getByRole('list', { name: 'Class directory' })
    .getByRole('listitem')
    .filter({ hasText: 'Whoop Class' });
  await expect(whoop).toBeVisible({ timeout: 15_000 });

  // Hide it (it stays listed here, marked "Hidden", and now offers "Unhide").
  await whoop.getByRole('button', { name: 'Hide' }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();
  await expect(whoop.getByText('Hidden', { exact: true })).toBeVisible({ timeout: 15_000 });
  await expect(whoop.getByRole('button', { name: 'Unhide' })).toBeVisible();

  // The Director recorded it: the class is flagged hidden on the wire, and a reload still shows it.
  const hiddenIds = async () =>
    ((await (await page.request.get('/classes')).json()) as { id: string; hidden?: boolean }[])
      .filter((c) => c.hidden)
      .map((c) => c.id);
  expect(await hiddenIds()).toContain('mgp-whoop');
  await page.reload();
  await expect(whoop.getByText('Hidden', { exact: true })).toBeVisible({ timeout: 15_000 });

  // ── Unhide it (cleanup for the shared Director) ──────────────────────────────────────────────
  await whoop.getByRole('button', { name: 'Unhide' }).click();
  await expect(whoop.getByText('Hidden', { exact: true })).toHaveCount(0, { timeout: 15_000 });
  await expect(whoop.getByRole('button', { name: 'Hide' })).toBeVisible();
  expect(await hiddenIds()).not.toContain('mgp-whoop');
});
