/**
 * Marshaling a race through the console UI: the save options, the to-do list, lap rulings,
 * a protest, "looked at", and the lock.
 *
 * A person runs one bench heat in which Ace makes a stray extra crossing, and saves it with
 * "Needs a look" and a note. On the Marshal page the race is in the to-do list. They remove
 * the stray lap and see the lap times and the heat's ranking change, restore it, mark a lap
 * as a restart and unmark it, file a protest and resolve it, and mark the race as looked at.
 * Then they lock the heat: the race is read-only and the Director refuses a ruling on it,
 * until the heat is unlocked again.
 *
 * Everything is the race-day path: real commands to the real Director, the real change feed,
 * nothing mocked. The bench has no signal, so the trace is the one plain sentence; the trace
 * itself is covered by the component tests.
 *
 * The spec also checks the display rule: no wire handle (a race id's `#1`, the `bench`
 * timer, a seat handle, the phase id, a pilot id, a protest id) is anywhere on the page.
 *
 * The worker's Director is shared, so every name here is unique and the spec deletes what it
 * made and puts the active event back.
 */
import type { Locator, Page } from '@playwright/test';
import { expect, test } from './observability.js';
import { gotoEvents, gotoPage, heatOnTimer, openPage, pagesNav } from './nav.js';

const STAMP = Date.now();
const EVENT = `Review Night ${STAMP}`;
const HEAT = `Review Heat ${STAMP}`;
const ACE = `E2E-Mace-${STAMP}`;
const BEE = `E2E-Mbee-${STAMP}`;
const NOTE = `Ace may have a stray lap ${STAMP}`;
const RACE_NAME = `${HEAT} · race 1`;

/** Add a pilot on the Pilots page with only a callsign. */
async function addPilot(page: Page, callsign: string): Promise<void> {
  await page.getByRole('button', { name: '+ Add pilot' }).click();
  const form = page.getByRole('form', { name: 'Add pilot' });
  await form.getByLabel('Callsign').fill(callsign);
  await page.getByRole('button', { name: 'Add pilot', exact: true }).click();
  await expect(
    page.getByRole('list', { name: 'Pilot directory' }).getByRole('listitem').filter({
      hasText: callsign
    })
  ).toBeVisible({ timeout: 15_000 });
}

/** The lap time shown in one row of a seat's lap table. */
const lapTime = (seat: Locator, row: number): Locator =>
  seat.getByRole('table').locator('tbody tr').nth(row).locator('[data-lap-time]');

/** The lap count shown on a seat. */
const lapCount = (seat: Locator): Locator => seat.locator('[data-stat="laps"]');

/** One cell of a seat's lap table on the Marshal page: row 1 is the hole shot. */
const lapCell = (seat: Locator, row: number, column: number): Locator =>
  seat.getByRole('table').locator('tbody tr').nth(row).locator('th, td').nth(column);

test('a marshal reviews a flagged race: lap rulings, a protest, looked at, and the lock', async ({
  page
}) => {
  // What was active before, so it can be put back at the end.
  const before = (await (await page.request.get('/active-event')).json()) as {
    event: { id: string } | null;
  };
  let eventId: string | undefined;

  try {
    // ── Two pilots, an event, and one bench heat ─────────────────────────────────────────
    await gotoPage(page, 'Pilots');
    await addPilot(page, ACE);
    await addPilot(page, BEE);

    await gotoEvents(page);
    await page
      .getByRole('button', { name: /New event/ })
      .first()
      .click();
    await page.getByRole('textbox', { name: 'Event name' }).fill(EVENT);
    await page.getByRole('button', { name: /Create & enter/ }).click();
    await expect(page.getByRole('heading', { name: EVENT, level: 1 })).toBeVisible({
      timeout: 15_000
    });
    const active = (await (await page.request.get('/active-event')).json()) as {
      event: { id: string } | null;
    };
    eventId = active.event?.id;
    expect(eventId).toBeTruthy();

    await openPage(page, 'Phases');
    await page.getByRole('button', { name: '+ Add heat' }).click();
    const form = page.getByRole('form', { name: 'Add a heat' });
    await form.getByLabel('Heat name').fill(HEAT);
    await form.getByRole('button', { name: ACE, exact: true }).click();
    await form.getByRole('button', { name: BEE, exact: true }).click();
    await form.getByLabel('Format').selectOption({ label: 'First to 3 laps' });
    await page.getByRole('button', { name: 'Add heat', exact: true }).click();
    await expect(form).toBeHidden({ timeout: 15_000 });

    const phase = page
      .getByRole('list', { name: 'Phases' })
      .locator('li.phase')
      .filter({ hasText: HEAT });
    await expect(phase).toContainText('Ready', { timeout: 15_000 });
    // Nothing needs a look yet, so the Marshal entry in the row carries no count.
    await expect(page.getByTestId('marshal-count')).toHaveCount(0);

    // ── Run the race. Ace's second crossing after the hole shot is the stray one. ────────
    // The heat is first in the run queue; run it from there.
    await page.getByRole('button', { name: `Run ${HEAT} next` }).click();
    await expect(heatOnTimer(page)).toHaveText(HEAT);
    const status = page.getByTestId('run-status');
    const runAce = page.getByRole('listitem', { name: `Seat 1, ${ACE}` });
    const runBee = page.getByRole('listitem', { name: `Seat 2, ${BEE}` });
    const aceCrossing = runAce.getByRole('button', { name: 'Crossing for Seat 1' });
    const beeCrossing = runBee.getByRole('button', { name: 'Crossing for Seat 2' });
    await page.getByRole('button', { name: 'Start', exact: true }).click();
    const stop = page.getByRole('button', { name: 'Stop', exact: true });
    await expect(stop).toBeVisible({ timeout: 20_000 });

    await aceCrossing.click();
    await expect(runAce.getByText('On course')).toBeVisible();
    await beeCrossing.click();
    await expect(runBee.getByText('On course')).toBeVisible();
    // Ace: three laps, the second of them the stray. Ace is first to 3.
    for (const laps of [1, 2, 3]) {
      await page.waitForTimeout(300);
      await aceCrossing.click();
      await expect(lapCount(runAce)).toHaveText(String(laps));
    }
    // Bee: three honest laps, finishing after Ace.
    for (const laps of [1, 2, 3]) {
      await page.waitForTimeout(300);
      await beeCrossing.click();
      await expect(lapCount(runBee)).toHaveText(String(laps));
    }
    await expect(runAce.locator('[data-stat="place"]')).toHaveText('P1');
    await stop.click();
    await expect(status).toHaveText('Stopped. Save the race or discard it.');

    // ── Save with "Needs a look" and a note ──────────────────────────────────────────────
    await page.getByText('Save options').click();
    await page.getByRole('checkbox', { name: 'Needs a look' }).check();
    await page.getByRole('textbox', { name: 'Note for this race' }).fill(NOTE);
    await page.getByRole('button', { name: 'Save', exact: true }).click();
    await expect(status).toHaveText('This heat is complete.', { timeout: 15_000 });
    const earlier = page.getByRole('list', { name: 'Earlier races' }).getByRole('listitem');
    await expect(earlier.first()).toContainText('Needs a look');
    await expect(earlier.first()).toContainText(NOTE);

    // ── The row of pages counts it; the Marshal page lists it with why ───────────────────
    await expect(page.getByTestId('marshal-count')).toHaveText('1', { timeout: 15_000 });
    const marshalLink = pagesNav(page).getByRole('link', { name: /^Marshal/ });
    await marshalLink.click();
    await expect(page).toHaveURL(/#\/event\/marshal$/);
    await expect(page.getByRole('heading', { name: 'Marshal', level: 1 })).toBeVisible();
    await expect(marshalLink).toHaveAttribute('aria-current', 'page');
    // With no race named, the page opens on the one that needs a look.
    await expect(page.getByTestId('review-title')).toHaveText(RACE_NAME);

    const todo = page.getByRole('list', { name: 'Races that need a look' });
    const todoItem = todo.getByRole('listitem').filter({ hasText: RACE_NAME });
    await expect(todoItem).toContainText('Needs a look.');
    await expect(todoItem).toContainText(NOTE);
    await todoItem.getByRole('button').click();
    await expect(page).toHaveURL(/#\/event\/marshal\/.+/);
    await expect(page.getByTestId('review-title')).toHaveText(RACE_NAME);
    await expect(page.getByText(`Note: ${NOTE}`)).toBeVisible();

    const ace = page.getByRole('listitem', { name: `Seat 1, ${ACE}` });
    const bee = page.getByRole('listitem', { name: `Seat 2, ${BEE}` });
    const ranking = page.getByRole('list', { name: 'Heat ranking' }).getByRole('listitem');
    await expect(ace).toContainText('Seat 1 · Bench');
    await expect(lapCount(ace)).toHaveText(/3\s+laps/);
    await expect(lapCount(bee)).toHaveText(/3\s+laps/);
    await expect(ace.locator('[data-stat="place"]')).toHaveText('1st');
    await expect(ranking.nth(0)).toContainText(ACE);
    await expect(ranking.nth(1)).toContainText(BEE);
    // The bench has no signal: one plain sentence, and the rest of the page works.
    await expect(ace.getByTestId('no-signal')).toBeVisible();
    await expect(ace.getByRole('img')).toHaveCount(0);

    // Rows: 0 the hole shot, then laps 1 to 3. Column 2 is when it was crossed.
    await expect(ace.getByRole('table').locator('tbody tr')).toHaveCount(4);
    const lap1Time = await lapTime(ace, 1).innerText();
    const lap1At = await lapCell(ace, 1, 2).innerText();
    const lap3At = await lapCell(ace, 3, 2).innerText();

    // ── Remove the stray lap: lap 1 stays, and the next lap starts where lap 1 ended ─────
    await ace.getByRole('button', { name: `Remove lap 2 of ${ACE}` }).click();
    await page
      .getByRole('group', { name: 'Why remove lap 2?' })
      .getByRole('button', { name: 'False detection' })
      .click();
    await expect(lapCount(ace)).toHaveText(/2\s+laps/, { timeout: 15_000 });
    await expect(ace.getByRole('table').locator('tbody tr')).toHaveCount(3);
    await expect(lapTime(ace, 1)).toHaveText(lap1Time);
    await expect(lapCell(ace, 1, 2)).toHaveText(lap1At);
    // The lap that is now lap 2 ends where the old lap 3 ended, so it holds both times.
    await expect(lapCell(ace, 2, 2)).toHaveText(lap3At);
    const uncounted = ace.getByRole('list', { name: `Crossings of ${ACE} that do not count` });
    await expect(uncounted).toContainText('Removed by a marshal: false detection.');
    // Ace no longer has 3 laps, so Bee wins the race and leads the heat.
    await expect(bee.locator('[data-stat="place"]')).toHaveText('1st');
    await expect(ace.locator('[data-stat="place"]')).toHaveText('2nd');
    await expect(ranking.nth(0)).toContainText(BEE);
    await expect(ranking.nth(1)).toContainText(ACE);

    // ── Restore it: everything is as it was ──────────────────────────────────────────────
    await uncounted.getByRole('button', { name: /Restore/ }).click();
    await expect(lapCount(ace)).toHaveText(/3\s+laps/, { timeout: 15_000 });
    await expect(uncounted).toHaveCount(0);
    await expect(lapCell(ace, 3, 2)).toHaveText(lap3At);
    await expect(ranking.nth(0)).toContainText(ACE);

    // ── A restart is the other case: that lap has no time, the laps around it stay ───────
    await ace.getByRole('button', { name: `Mark lap 2 of ${ACE} as a restart` }).click();
    await expect(lapCount(ace)).toHaveText(/2\s+laps/, { timeout: 15_000 });
    await expect(ace.getByRole('table').locator('tbody tr')).toHaveCount(4);
    await expect(lapCell(ace, 2, 0)).toHaveText('Restart');
    await expect(lapCell(ace, 2, 1)).toHaveText('No lap time');
    await expect(lapTime(ace, 1)).toHaveText(lap1Time);
    await ace.getByRole('button', { name: `Unmark the restart of ${ACE}` }).click();
    await expect(lapCount(ace)).toHaveText(/3\s+laps/, { timeout: 15_000 });

    // ── File a protest. It joins the reasons on the to-do list. ──────────────────────────
    const protestForm = page.getByRole('form', { name: 'File a protest' });
    await protestForm.getByRole('combobox', { name: 'Protest by' }).selectOption({ label: BEE });
    await protestForm.getByRole('textbox', { name: 'What is protested' }).fill('Ace cut a gate');
    await protestForm.getByRole('button', { name: 'File protest' }).click();
    const protest = page
      .getByRole('list', { name: 'Protests against this race' })
      .getByRole('listitem', { name: 'Protest 1' });
    await expect(protest).toContainText('Open', { timeout: 15_000 });
    await expect(protest).toContainText(`By ${BEE}`);
    await expect(protest).toContainText('Ace cut a gate');
    await expect(todoItem).toContainText('1 open protest.');

    // ── No wire handle is on the Marshal page ────────────────────────────────────────────
    const state = (await (await page.request.get(`/events/${eventId}/state`)).json()) as {
      phases: { def: { id: string }; ranking: { pilot: string }[] }[];
      races: { race: string; seats: { laps: { crossing: number }[] }[] }[];
      protests: { protest: string }[];
    };
    const phaseId = state.phases[0].def.id;
    const raceId = state.races[0].race;
    const pilotIds = state.phases[0].ranking.map((placed) => placed.pilot);
    expect(pilotIds).toHaveLength(2);
    expect(state.protests).toHaveLength(1);
    const raw = [
      phaseId,
      raceId,
      '#1',
      'bench',
      'seat-1',
      'seat-2',
      state.protests[0].protest,
      ...pilotIds
    ];
    const marshalText = await page.locator('body').innerText();
    for (const handle of raw) expect(marshalText).not.toContain(handle);

    // ── A heat with an open protest cannot be locked: the Director says why ──────────────
    await openPage(page, 'Phases');
    await expect(phase).toContainText('Complete', { timeout: 15_000 });
    await phase.getByRole('button', { name: `Lock ${HEAT}` }).click();
    const refusal = page.getByRole('alert').filter({ hasText: 'That didn’t work.' });
    await expect(refusal).toBeVisible({ timeout: 15_000 });
    await expect(refusal).toContainText(/protest/i);
    await expect(refusal).toContainText(HEAT);
    // The refusal is the Director's sentence, shown as it is. It names nothing by id.
    for (const handle of raw) {
      await expect(refusal).not.toContainText(handle);
    }
    await expect(phase).not.toContainText('Locked');

    // ── Resolve the protest, then mark the race as looked at ─────────────────────────────
    await marshalLink.click();
    await todoItem.getByRole('button').click();
    await protest
      .getByRole('textbox', { name: 'Note for the outcome of protest 1' })
      .fill('Video shows a clean pass');
    await protest.getByRole('button', { name: 'Denied: protest 1' }).click();
    await expect(protest).toContainText('Denied', { timeout: 15_000 });
    await expect(protest.getByRole('button')).toHaveCount(0);
    await expect(todoItem).not.toContainText('open protest');

    await page.getByRole('button', { name: 'Looked at', exact: true }).click();
    await expect(page.getByText('Nothing needs a look.')).toBeVisible({ timeout: 15_000 });
    const others = page.getByRole('region', { name: HEAT });
    await expect(others.getByRole('listitem')).toHaveCount(1);
    // A quiet button puts it back, and "Looked at" clears it again.
    await page.getByRole('button', { name: 'Needs a look', exact: true }).click();
    await expect(todoItem).toContainText('Needs a look.', { timeout: 15_000 });
    await page.getByRole('button', { name: 'Looked at', exact: true }).click();
    await expect(page.getByText('Nothing needs a look.')).toBeVisible({ timeout: 15_000 });

    // ── Lock the heat: the race is official and read-only ────────────────────────────────
    await openPage(page, 'Phases');
    await expect(page.getByTestId('marshal-count')).toHaveCount(0);
    await phase.getByRole('button', { name: `Lock ${HEAT}` }).click();
    await expect(phase).toContainText('Locked', { timeout: 15_000 });
    await expect(phase.getByRole('button', { name: `Unlock ${HEAT}` })).toBeVisible();

    await marshalLink.click();
    await others.getByRole('button', { name: /race 1/ }).click();
    await expect(page.getByTestId('review-status')).toContainText(
      `${HEAT} is locked, so this race is official and cannot be changed.`
    );
    await expect(ace.getByRole('table').locator('tbody tr')).toHaveCount(4);
    await expect(page.getByRole('button', { name: /^Remove / })).toHaveCount(0);
    await expect(page.getByRole('button', { name: /as a restart$/ })).toHaveCount(0);
    await expect(page.getByRole('button', { name: /^False start by/ })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Looked at', exact: true })).toHaveCount(0);
    await expect(page.getByRole('form', { name: 'File a protest' })).toHaveCount(0);
    // The page offers no ruling, and the Director refuses one sent anyway.
    const crossing = state.races[0].seats[0].laps[1].crossing;
    const refused = await page.request.post(`/events/${eventId}/commands`, {
      data: { Record: { LapRemoved: { race: raceId, crossing } } }
    });
    expect(refused.status()).toBe(409);
    const lockedText = await page.locator('body').innerText();
    for (const handle of raw) expect(lockedText).not.toContain(handle);

    // ── Unlock: rulings are offered again ────────────────────────────────────────────────
    await openPage(page, 'Phases');
    await phase.getByRole('button', { name: `Unlock ${HEAT}` }).click();
    await expect(phase).toContainText('Complete', { timeout: 15_000 });
    const eventText = await page.locator('body').innerText();
    for (const handle of raw) expect(eventText).not.toContain(handle);
    await marshalLink.click();
    await others.getByRole('button', { name: /race 1/ }).click();
    await expect(ace.getByRole('button', { name: `Remove lap 2 of ${ACE}` })).toBeVisible({
      timeout: 15_000
    });

    // ── The page survives a reload on the chosen race ────────────────────────────────────
    await page.reload();
    await expect(page.getByTestId('review-title')).toHaveText(RACE_NAME, { timeout: 15_000 });
    await expect(lapCount(ace)).toHaveText(/3\s+laps/);
  } finally {
    // ── Clean up: the event, the two pilots, and the active event as it was ──────────────
    if (eventId) await page.request.delete(`/events/${eventId}`);
    const pilots = (await (await page.request.get('/pilots')).json()) as {
      id: string;
      callsign: string;
    }[];
    for (const pilot of pilots.filter((p) => p.callsign === ACE || p.callsign === BEE)) {
      await page.request.delete(`/pilots/${pilot.id}`);
    }
    if (before.event) {
      await page.request.put('/active-event', { data: { id: before.event.id } });
    }
  }
});
