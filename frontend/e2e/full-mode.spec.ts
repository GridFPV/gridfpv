/**
 * A solo home session through every page of full mode, start to result.
 *
 * One pilot times themself on a home track, the way a RaceGOW evening goes:
 *
 *   1. **Details**: set the minimum lap time once, for the whole event.
 *   2. **Formats**: make a format in the editor.
 *   3. **Phases**: add a phase with that format, and a heat under it that uses the phase's.
 *   4. **Run**: start with the keyboard, fly laps (one crossing comes too soon), stop, save.
 *   5. **Marshal**: the too-soon crossing is listed with why, and is ruled to count.
 *   6. **Results**: the phase's ranking, provisional.
 *   7. **Help**: the page's `?` opens its help, built in.
 *
 * No timer is wired in, so the crossings come from the Run page's bench buttons, which post
 * real passes to the real Director. Everything else is the real path: real commands, the
 * real change feed, the event's lap rules applied by the Director. Nothing is mocked.
 *
 * The worker's Director is shared, so every name here is unique and the spec deletes what it
 * made and puts the active event back.
 */
import type { Page } from '@playwright/test';
import { heatOnTimer, openPage, pagesNav } from './nav.js';
import { expect, test } from './observability.js';

const STAMP = Date.now();
const EVENT = `Home Track ${STAMP}`;
const FORMAT = `First to two ${STAMP}`;
const PHASE = `Track One ${STAMP}`;
const HEAT = `Runs ${STAMP}`;
const ACE = `E2E-Solo-${STAMP}`;

const MIN_LAP_SECONDS = 2;

async function createPilot(page: Page, callsign: string): Promise<string> {
  const res = await page.request.post('/pilots', { data: { callsign } });
  expect(res.ok()).toBe(true);
  return ((await res.json()) as { id: string }).id;
}

async function createEvent(page: Page, name: string): Promise<string> {
  const res = await page.request.post('/events', { data: { name } });
  expect(res.ok()).toBe(true);
  return ((await res.json()) as { id: string }).id;
}

interface State {
  lap_rules: { min_lap_micros?: number; ignore_before_micros?: number };
  phases: { def: { id: string; label: string; parent?: string; format?: { name: string } } }[];
  races: { state: string; seats: { laps: unknown[]; uncounted: { why: string }[] }[] }[];
}

test('one pilot runs a home session in full mode: lap rule, format, phase, race, ruling, result', async ({
  page
}) => {
  const before = (await (await page.request.get('/active-event')).json()) as {
    event: { id: string } | null;
  };
  const pilotId = await createPilot(page, ACE);
  const eventId = await createEvent(page, EVENT);
  const state = async (): Promise<State> =>
    (await (await page.request.get(`/events/${eventId}/state`)).json()) as State;

  try {
    expect((await page.request.put('/active-event', { data: { id: eventId } })).ok()).toBe(true);

    // ── Details: the minimum lap, set once. It saves when the field is left. ───────────────
    await page.goto('/#/event');
    await expect(page.getByRole('heading', { name: EVENT, level: 1 })).toBeVisible({
      timeout: 15_000
    });
    const minLap = page.getByLabel('Minimum lap time, in seconds');
    await expect(minLap).toHaveValue('');
    await minLap.fill(String(MIN_LAP_SECONDS));
    await minLap.blur();
    await expect(page.getByTestId('lap-rules-state')).toContainText('Saved.', {
      timeout: 15_000
    });
    expect((await state()).lap_rules).toEqual({ min_lap_micros: MIN_LAP_SECONDS * 1_000_000 });
    // It is the Director's, so a reload shows it.
    await page.reload();
    await expect(minLap).toHaveValue(String(MIN_LAP_SECONDS), { timeout: 15_000 });

    // ── Formats: a table, and an editor that says the format back in words ───────────────
    await openPage(page, 'Formats');
    const formats = page.getByRole('table', { name: 'Formats' });
    await expect(formats.getByRole('row').filter({ hasText: 'Best 3 consecutive' })).toContainText(
      'Ranked by fastest 3 laps in a row'
    );
    await page.getByRole('button', { name: '+ New format' }).click();
    const editor = page.getByRole('form', { name: 'New format' });
    await editor.getByLabel('Format name').fill(FORMAT);
    await editor.getByLabel('The race ends').selectOption('Laps');
    await editor.getByLabel('Laps to win').fill('2');
    // No countdown, so the race starts the moment Start is pressed.
    await editor.getByLabel('Countdown, in seconds').fill('0');
    await editor.getByLabel('The heat is done').selectOption('Director');
    await editor.getByLabel('Ranked by').selectOption('best-lap');
    await expect(editor.getByTestId('format-reads')).toHaveText(
      'First to 2 laps · Ranked by fastest lap · Until you call it done'
    );
    await page.getByRole('button', { name: 'Save format' }).click();
    await expect(editor).toBeHidden({ timeout: 15_000 });
    const made = formats.getByRole('row').filter({ hasText: FORMAT });
    await expect(made).toContainText('First to 2 laps');
    await expect(made).toContainText('Starts at once');
    // The same name again is refused, in the Director's words, and the dialog stays open.
    await made.getByRole('button', { name: `Duplicate ${FORMAT}` }).click();
    const copy = page.getByRole('form', { name: 'New format' });
    await expect(copy.getByLabel('Format name')).toHaveValue(`${FORMAT} copy`);
    await copy.getByLabel('Format name').fill(FORMAT);
    await page.getByRole('button', { name: 'Save format' }).click();
    await expect(copy.getByRole('alert')).toContainText(FORMAT);
    await page.getByRole('button', { name: 'Cancel' }).click();
    await expect(copy).toBeHidden();

    // ── Phases: a phase with the format, then a heat under it that uses the phase's ──────
    await openPage(page, 'Phases');
    await expect(page.getByTestId('no-phases')).toBeVisible();
    await page.getByRole('button', { name: '+ Add phase' }).click();
    const phaseForm = page.getByRole('form', { name: 'Add a phase' });
    await phaseForm.getByLabel('Phase name').fill(PHASE);
    await phaseForm.getByLabel('Format').selectOption({ label: FORMAT });
    await page.getByRole('button', { name: 'Add phase', exact: true }).click();
    await expect(phaseForm).toBeHidden({ timeout: 15_000 });

    const tree = page.getByRole('list', { name: 'Phases' });
    const phaseRow = tree.locator('li.phase').filter({ hasText: PHASE }).first();
    await expect(phaseRow).toContainText(FORMAT);
    await phaseRow.getByRole('button', { name: `Add a heat under ${PHASE}` }).click();
    const heatForm = page.getByRole('form', { name: 'Add a heat' });
    await expect(heatForm.getByLabel('Under').locator('option:checked')).toHaveText(PHASE);
    // The format box starts on the phase's, so the heat carries none of its own.
    await expect(heatForm.getByLabel('Format').locator('option:checked')).toHaveText(
      `The phase's: ${FORMAT}`
    );
    await heatForm.getByLabel('Heat name').fill(HEAT);
    await heatForm.getByRole('button', { name: ACE, exact: true }).click();
    await page.getByRole('button', { name: 'Add heat', exact: true }).click();
    await expect(heatForm).toBeHidden({ timeout: 15_000 });
    const heatRow = tree.locator('li.phase.heat').filter({ hasText: HEAT });
    await expect(heatRow).toContainText('Ready', { timeout: 15_000 });
    await expect(heatRow).toContainText(FORMAT);
    const defs = (await state()).phases.map((phase) => phase.def);
    const heatDef = defs.find((def) => def.label === HEAT);
    const phaseDef = defs.find((def) => def.label === PHASE);
    expect(heatDef?.parent).toBe(phaseDef?.id);
    expect(heatDef?.format).toBeUndefined();
    expect(phaseDef?.format?.name).toBe(FORMAT);

    // ── Run: with no heat named, the page is on the heat that is up next ─────────────────
    await openPage(page, 'Run');
    await expect(page).toHaveURL(/#\/event\/run$/);
    await expect(heatOnTimer(page)).toHaveText(HEAT);
    await expect(
      page.getByRole('combobox', { name: 'Phase' }).locator('option:checked')
    ).toHaveText(PHASE);
    await expect(page.getByTestId('run-rules')).toContainText(FORMAT);
    const status = page.getByTestId('run-status');
    await expect(status).toHaveText('Ready to start.');
    const seat = page.getByRole('listitem', { name: `Seat 1, ${ACE}` });
    const crossing = seat.getByRole('button', { name: 'Crossing for Seat 1' });
    const laps = seat.locator('[data-stat="laps"]');

    // The keyboard: "?" shows the list, Space starts.
    await page.keyboard.press('?');
    await expect(page.getByTestId('run-keys')).toContainText('Start, then Stop, then Save');
    await page.keyboard.press('Space');
    await expect(status).toHaveText('Racing.', { timeout: 15_000 });

    // The hole shot, a lap, a crossing that comes too soon, then a second lap.
    await crossing.click();
    await expect(seat.getByText('On course')).toBeVisible();
    await page.waitForTimeout(MIN_LAP_SECONDS * 1000 + 300);
    await crossing.click();
    await expect(laps).toHaveText('1');
    // Under the event's minimum lap: the Director keeps it and does not count it.
    await page.waitForTimeout(300);
    await crossing.click();
    await page.waitForTimeout(MIN_LAP_SECONDS * 1000 + 300);
    await crossing.click();
    await expect(laps).toHaveText('2');
    await expect(
      seat.getByRole('list', { name: 'Laps for Seat 1' }).getByRole('listitem')
    ).toHaveCount(2);

    // "x" stops, "s" saves. Nothing is saved by itself.
    await page.keyboard.press('x');
    await expect(status).toHaveText('Stopped. Save the race or discard it.');
    await page.keyboard.press('s');
    await expect(status).toHaveText('Ready for the next race.', { timeout: 15_000 });
    const raced = await state();
    expect(raced.races.map((race) => race.state)).toEqual(['Saved']);
    expect(raced.races[0].seats[0].laps).toHaveLength(2);
    expect(raced.races[0].seats[0].uncounted.map((left) => left.why)).toEqual(['UnderMinLap']);

    // ── Marshal: opens on the race just flown; the left-out crossing can be counted ──────
    await pagesNav(page)
      .getByRole('link', { name: /^Marshal/ })
      .click();
    await expect(page.getByTestId('review-title')).toHaveText(`${HEAT} · race 1`, {
      timeout: 15_000
    });
    const marshalSeat = page.getByRole('listitem', { name: `Seat 1, ${ACE}` });
    const leftOut = marshalSeat.getByRole('list', {
      name: `Crossings of ${ACE} that do not count`
    });
    await expect(leftOut.getByRole('listitem')).toHaveCount(1);
    await expect(leftOut).toContainText('under the minimum lap time');
    await expect(marshalSeat.getByRole('table').locator('tbody tr')).toHaveCount(3);
    await leftOut.getByRole('button', { name: /^Count the crossing of / }).click();
    // Hole shot plus three laps now, and nothing left out.
    await expect(marshalSeat.getByRole('table').locator('tbody tr')).toHaveCount(4, {
      timeout: 15_000
    });
    await expect(leftOut).toHaveCount(0);
    expect((await state()).races[0].seats[0].laps).toHaveLength(3);

    // ── Results: the phase's ranking, provisional until it is locked ─────────────────────
    await openPage(page, 'Results');
    const results = page.getByRole('table', { name: `Results of ${PHASE}` });
    await expect(results.getByRole('row').nth(1)).toContainText(ACE, { timeout: 15_000 });
    await expect(page.getByTestId('result-state')).toContainText('Provisional');

    // ── Help: the "?" beside the title opens that page's help, built into the console ────
    await page.getByRole('link', { name: 'Help for Results' }).click();
    await expect(page).toHaveURL(/#\/help\/results$/);
    await expect(page.getByRole('heading', { name: 'Results', level: 1 })).toBeVisible();
    await expect(page.getByRole('article')).toContainText('provisional');
    await page.getByRole('link', { name: 'All help pages' }).click();
    await expect(
      page.getByRole('list', { name: 'Help pages' }).getByRole('listitem')
    ).not.toHaveCount(0);

    // ── No wire handle is on a page ──────────────────────────────────────────────────────
    const handles = [pilotId, eventId, heatDef!.id, phaseDef!.id, 'seat-1'];
    for (const [name, title] of [
      ['Phases', 'Phases'],
      ['Run', 'Run'],
      ['Results', 'Results']
    ]) {
      await openPage(page, name, title);
      const text = await page.locator('body').innerText();
      for (const handle of handles) expect(text, `${name} page`).not.toContain(handle);
    }
  } finally {
    await page.request.delete(`/events/${eventId}`);
    await page.request.delete(`/pilots/${pilotId}`);
    const formats = (await (await page.request.get('/formats')).json()) as {
      id: string;
      format: { name: string };
    }[];
    for (const saved of formats.filter((f) => f.format.name.startsWith(FORMAT))) {
      await page.request.delete(`/formats/${encodeURIComponent(saved.id)}`);
    }
    if (before.event) {
      await page.request.put('/active-event', { data: { id: before.event.id } });
    }
  }
});
