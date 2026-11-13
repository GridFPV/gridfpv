/**
 * Running a heat through the console UI, start to saved result.
 *
 * A person adds two pilots, creates an event, adds a heat on the Phases page with the "First
 * to 3 laps" format, opens it on the Run page and runs one race: Start, the countdown, crossings on each seat, Stop,
 * Save. The heat is then complete and its ranking shows the two callsigns in finishing order.
 *
 * No timer is wired in yet, so the crossings come from the Run view's "Crossing" bench
 * buttons, which post real passes to the real Director. Everything else is the race-day path:
 * real commands, the real change feed, nothing mocked.
 *
 * The spec also checks the display rule: no wire handle (a race id's `#1`, the `bench`
 * timer, a seat handle, the phase id, a pilot id) is anywhere on the page.
 *
 * The worker's Director is shared, so every name here is unique and the spec deletes what it
 * made and puts the active event back.
 */
import type { Locator, Page } from '@playwright/test';
import { expect, test } from './observability.js';
import { gotoEvents, gotoPage, heatOnTimer, openEvent, openPage } from './nav.js';

const STAMP = Date.now();
const EVENT = `Run Heat Night ${STAMP}`;
const HEAT = `Quali Heat ${STAMP}`;
const ACE = `E2E-Ace-${STAMP}`;
const BEE = `E2E-Bee-${STAMP}`;

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

/** The lap count shown on a seat card. */
const lapCount = (seat: Locator): Locator => seat.locator('[data-stat="laps"]');

test('RD adds a heat, runs one race with bench crossings, saves it, and sees the ranking', async ({
  page
}) => {
  // What was active before, so it can be put back at the end.
  const before = (await (await page.request.get('/active-event')).json()) as {
    event: { id: string } | null;
  };
  let eventId: string | undefined;

  try {
    // ── Two pilots, through the Pilots page ──────────────────────────────────────────────
    await gotoPage(page, 'Pilots');
    await addPilot(page, ACE);
    await addPilot(page, BEE);

    // ── A new event ──────────────────────────────────────────────────────────────────────
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
    await expect(page.getByText('Nothing to race yet.')).toBeVisible();

    // ── Add a heat: Ace on Seat 1, Bee on Seat 2, first to 3 laps ────────────────────────
    await page.getByRole('button', { name: '+ Add heat' }).click();
    const form = page.getByRole('form', { name: 'Add a heat' });
    await form.getByLabel('Heat name').fill(HEAT);
    await form.getByRole('button', { name: ACE, exact: true }).click();
    await form.getByRole('button', { name: BEE, exact: true }).click();
    await form.getByLabel('Format').selectOption({ label: 'First to 3 laps' });
    await page.getByRole('button', { name: 'Add heat', exact: true }).click();
    await expect(form).toBeHidden({ timeout: 15_000 });

    // The heat is in the phase list, ready, with its lineup by seat and callsign.
    const phase = page
      .getByRole('list', { name: 'Phases' })
      .locator('li.phase')
      .filter({ hasText: HEAT });
    await expect(phase).toContainText('Ready', { timeout: 15_000 });
    const lineup = phase.getByRole('list', { name: `Lineup of ${HEAT}` });
    await expect(lineup.getByRole('listitem').nth(0)).toHaveText(`Seat 1 ${ACE}`);
    await expect(lineup.getByRole('listitem').nth(1)).toHaveText(`Seat 2 ${BEE}`);

    // ── Open the Run view ────────────────────────────────────────────────────────────────
    await phase.getByRole('button', { name: `Run ${HEAT}` }).click();
    await expect(page).toHaveURL(/#\/event\/run\//);
    await expect(page.getByRole('heading', { name: 'Run', level: 1 })).toBeVisible();
    // The heat is on the timer, with its format and what times it said in words.
    await expect(heatOnTimer(page)).toHaveText(HEAT);
    await expect(openEvent(page)).toContainText(EVENT);
    await expect(page.getByTestId('run-rules')).toContainText('First to 3 laps');
    await expect(page.getByTestId('timed-by')).toHaveText('Timed by Bench');
    const status = page.getByTestId('run-status');
    await expect(status).toHaveText('Ready to start.');

    const ace = page.getByRole('listitem', { name: `Seat 1, ${ACE}` });
    const bee = page.getByRole('listitem', { name: `Seat 2, ${BEE}` });
    const aceCrossing = ace.getByRole('button', { name: 'Crossing for Seat 1' });
    const beeCrossing = bee.getByRole('button', { name: 'Crossing for Seat 2' });
    // The bench tool is marked as one, and does nothing until a race is running.
    await expect(ace.getByText('Bench', { exact: true })).toBeVisible();
    await expect(aceCrossing).toBeDisabled();

    // ── Start: a 5 second countdown, then the race is running ────────────────────────────
    await page.getByRole('button', { name: 'Start', exact: true }).click();
    await expect(status).toHaveText('Counting down to the start.');
    await expect(page.getByRole('button', { name: 'Call off' })).toBeVisible();
    await expect(page.getByRole('timer')).toHaveAttribute('data-mode', 'remaining');
    const stop = page.getByRole('button', { name: 'Stop', exact: true });
    await expect(stop).toBeVisible({ timeout: 20_000 });
    await expect(status).toHaveText('Racing.');

    // ── Crossings. The first one on a seat starts lap 1; each one after ends a lap. ──────
    // Ace always crosses before Bee, so Ace is first to 3 laps.
    await aceCrossing.click();
    await expect(ace.getByText('On course')).toBeVisible();
    await beeCrossing.click();
    await expect(bee.getByText('On course')).toBeVisible();
    for (const laps of [1, 2, 3]) {
      await aceCrossing.click();
      await expect(lapCount(ace)).toHaveText(String(laps));
      await beeCrossing.click();
      await expect(lapCount(bee)).toHaveText(String(laps));
    }
    await expect(ace.locator('[data-stat="place"]')).toHaveText('P1');
    await expect(bee.locator('[data-stat="place"]')).toHaveText('P2');
    await expect(
      ace.getByRole('list', { name: 'Laps for Seat 1' }).getByRole('listitem')
    ).toHaveCount(3);

    // ── Stop, then save. Nothing is saved by itself. ─────────────────────────────────────
    await stop.click();
    await expect(status).toHaveText('Stopped. Save the race or discard it.');
    await expect(page.getByRole('button', { name: 'Discard', exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Save', exact: true }).click();

    // One saved race completes a "First to 3 laps" heat.
    await expect(status).toHaveText('This heat is complete.', { timeout: 15_000 });
    const ranking = page.getByRole('list', { name: 'Heat ranking' }).getByRole('listitem');
    await expect(ranking).toHaveCount(2);
    await expect(ranking.nth(0)).toContainText(ACE);
    await expect(ranking.nth(1)).toContainText(BEE);
    const earlier = page.getByRole('list', { name: 'Earlier races' }).getByRole('listitem');
    await expect(earlier).toHaveCount(1);
    await expect(earlier.first()).toContainText('Race 1');
    await expect(earlier.first()).toContainText('Saved');
    await expect(earlier.first()).toContainText(`1. ${ACE}`);

    // ── No wire handle is on the page ────────────────────────────────────────────────────
    const state = (await (await page.request.get(`/events/${eventId}/state`)).json()) as {
      phases: { def: { id: string }; complete: boolean; ranking: { pilot: string }[] }[];
      races: { race: string; state: string }[];
    };
    expect(state.phases).toHaveLength(1);
    expect(state.phases[0].complete).toBe(true);
    expect(state.races.map((race) => race.state)).toEqual(['Saved']);
    const phaseId = state.phases[0].def.id;
    const pilotIds = state.phases[0].ranking.map((placed) => placed.pilot);
    expect(pilotIds).toHaveLength(2);
    const raw = [phaseId, state.races[0].race, '#1', 'bench', 'seat-1', 'seat-2', ...pilotIds];
    const runText = await page.locator('body').innerText();
    for (const handle of raw) expect(runText).not.toContain(handle);

    // ── The Run view survives a reload, and the event page shows the heat complete ───────
    await page.reload();
    await expect(heatOnTimer(page)).toContainText(HEAT, { timeout: 15_000 });
    await expect(ranking.nth(0)).toContainText(ACE);
    await openPage(page, 'Phases');
    await expect(phase).toContainText('Complete', { timeout: 15_000 });

    // ── And the Results page has the heat's ranking, in the same order ───────────────────
    await openPage(page, 'Results');
    const results = page.getByRole('table', { name: `Results of ${HEAT}` }).getByRole('row');
    await expect(results.nth(1)).toContainText(ACE);
    await expect(results.nth(2)).toContainText(BEE);
    await expect(page.getByTestId('result-state')).toContainText('Provisional');
    const eventText = await page.locator('body').innerText();
    for (const handle of raw) expect(eventText).not.toContain(handle);
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
