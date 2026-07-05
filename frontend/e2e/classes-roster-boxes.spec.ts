/**
 * Classes / Pilots — the two labelled collapsible boxes on the Classes & Roster stage, plus the bulk
 * roster select-all and the per-class channel auto-assign (ui: Classes/Pilots boxes).
 *
 * The stage is restructured into exactly two top-level collapsible boxes — **Classes** and
 * **Pilots** — each persisting its open state per stable section id in `localStorage`
 * (gf.collapse.<event>.<section>). The per-class "Channels" card (inside Pilots) is where pilots get
 * channels, with its own per-class **Auto-assign channels** button (no standalone Channels box). This
 * spec drives the real UI against a real Director:
 *
 *   1. **Collapse each of the two boxes**, assert their content hides, reload, and assert both
 *      stayed collapsed (the persisted state stuck).
 *   2. Re-open them, **Select all** in the Pilots box and save the roster, then
 *   3. **Auto-assign channels** in the per-class Channels card and assert every placed pilot got a
 *      channel (its `<select>` carries a non-empty value).
 *
 * Nothing about the boxes / bulk actions is mocked. Cleans the shared Director's event back to empty.
 */
import { expect, test } from './observability.js';

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

/**
 * A box's top-level toggle (a Collapsible header button) by its title. Scoped to the stage region and
 * to *direct-child* collapsible toggles, so the nav item ("Classes & Roster") and the nested Roster /
 * placement collapsibles don't match.
 */
function boxToggle(page: import('@playwright/test').Page, title: string) {
  return page
    .getByRole('region', { name: 'Classes and roster' })
    .locator('> .gf-collapsible > .gf-collapsible-head > .gf-collapsible-toggle')
    .filter({ has: page.getByText(title, { exact: true }) });
}

test('two labelled boxes collapse + persist; select-all roster; per-class auto-assign channels', async ({
  page,
  director
}) => {
  const base = director.baseUrl;
  const ev = `${base}/events/practice`;
  const json = { headers: { 'Content-Type': 'application/json' } };
  const SUFFIX = Date.now();
  const PILOTS = [`E2E-Box-Ace-${SUFFIX}`, `E2E-Box-Bee-${SUFFIX}`, `E2E-Box-Cas-${SUFFIX}`];

  // ── Set up over the real write paths: Open Class selected, three directory pilots. ─────────────
  const classes = (await (await page.request.get(`${base}/classes`)).json()) as Array<{
    id: string;
    name: string;
  }>;
  const classId = classes.find((c) => c.name === 'Open Class')!.id;
  const pilotIds: string[] = [];
  for (const callsign of PILOTS) {
    const p = (await (
      await page.request.post(`${base}/pilots`, { ...json, data: { callsign } })
    ).json()) as { id: string };
    pilotIds.push(p.id);
  }
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [classId] } });

  await page.goto('/');
  await enterPractice(page);
  await openTab(page, 'Classes & Roster');

  // ── 1. The two labelled boxes exist; collapse each one and assert its body hides. ──────────────
  const classesBox = boxToggle(page, 'Classes');
  const pilotsBox = boxToggle(page, 'Pilots');
  for (const box of [classesBox, pilotsBox]) {
    await expect(box).toBeVisible({ timeout: 15_000 });
    await expect(box).toHaveAttribute('aria-expanded', 'true');
  }

  // Collapse both.
  for (const box of [classesBox, pilotsBox]) {
    await box.click();
    await expect(box).toHaveAttribute('aria-expanded', 'false');
  }
  if (process.env.GRIDFPV_SHOTS)
    await page
      .getByRole('region', { name: 'Classes and roster' })
      .screenshot({ path: `${process.env.GRIDFPV_SHOTS}/classes-roster-boxes-collapsed.png` });

  // ── It persisted: a reload keeps both boxes collapsed. ─────────────────────────────────────────
  await page.reload();
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await openTab(page, 'Classes & Roster');
  for (const title of ['Classes', 'Pilots']) {
    await expect(boxToggle(page, title)).toHaveAttribute('aria-expanded', 'false', {
      timeout: 15_000
    });
  }

  // ── 2. Re-open the boxes; Select all in the Pilots box — the roster AUTO-SAVES (no Save button). ─
  await boxToggle(page, 'Pilots').click();
  await expect(boxToggle(page, 'Pilots')).toHaveAttribute('aria-expanded', 'true');

  const rosterSaved = page.waitForResponse(
    (r) => /\/events\/.+\/roster$/.test(r.url()) && r.request().method() === 'PUT'
  );
  await page.getByRole('button', { name: 'Select all', exact: true }).click();
  // Every directory pilot's roster checkbox is now ticked.
  for (const callsign of PILOTS) {
    await expect(page.getByRole('checkbox', { name: `Roster ${callsign}` })).toBeChecked();
  }
  await rosterSaved;
  // Single-class auto-fill places every rostered pilot — their channel selectors appear.
  for (const callsign of PILOTS) {
    await expect(page.getByLabel(`Channel for ${callsign}`)).toBeVisible({ timeout: 15_000 });
    await expect(page.getByLabel(`Channel for ${callsign}`)).toHaveValue('');
  }
  if (process.env.GRIDFPV_SHOTS)
    await page
      .getByRole('region', { name: 'Classes and roster' })
      .screenshot({ path: `${process.env.GRIDFPV_SHOTS}/classes-roster-boxes-expanded.png` });

  // ── 3. Auto-assign channels in the per-class Channels card; every placed pilot gets a channel. ─
  // Single-class event → one "Auto-assign channels" button (in the lone class's Channels card).
  await page.getByRole('button', { name: 'Auto-assign channels' }).first().click();
  for (const callsign of PILOTS) {
    await expect(page.getByLabel(`Channel for ${callsign}`)).not.toHaveValue('', {
      timeout: 15_000
    });
  }
  if (process.env.GRIDFPV_SHOTS)
    await page
      .getByRole('region', { name: 'Classes and roster' })
      .screenshot({ path: `${process.env.GRIDFPV_SHOTS}/classes-roster-auto-assigned.png` });

  // ── Clean up the shared Director's event back to empty. ────────────────────────────────────────
  await page.request.put(`${ev}/classes/${classId}/membership`, { ...json, data: { pilots: [] } });
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [] } });
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
});
