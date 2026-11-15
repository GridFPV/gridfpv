/**
 * The replay overlay: a finished race shown as it stood at any moment, which is the page the
 * Director photographs to draw the overlay onto a clean recording.
 *
 * A race is flown through the API (real commands, real passes), saved, and then the replay
 * page is opened as the Director's own browser would open it: by URL, at 1920x1080, and
 * moved with `window.gridfpvReplay`. The spec checks that it counts down before the start,
 * shows only the laps flown by each moment, ends on the finished race, is bare and
 * see-through, and puts no wire handle on the page.
 *
 * The e2e Director has no browser of its own or ffmpeg to draw with, so the drawing itself
 * is proved by `crates/app/tests/render_live.rs`. What the console's Video page says about
 * an event with nothing recorded, and the Director's refusals, are checked here.
 */
import type { Page } from '@playwright/test';
import { gotoPage } from './nav.js';
import { expect, test } from './observability.js';

const STAMP = Date.now();
const EVENT = `Replay Night ${STAMP}`;
const HEAT_LABEL = `Replay Heat ${STAMP}`;
const HEAT = `replay-heat-${STAMP}`;
const ACE = `RP-Ace-${STAMP}`;

interface Handle {
  ready(): Promise<string>;
  seek(ms: number): Promise<string>;
}
type ReplayWindow = Window & { gridfpvReplay?: Handle };

interface Snapshot {
  races: {
    race: string;
    started_at?: number;
    stopped_at?: number;
    seats: { laps: { ended_at: number }[]; hole_shot?: { at: number } }[];
  }[];
}

async function post(page: Page, path: string, data: unknown): Promise<void> {
  const res = await page.request.post(path, { data });
  expect(res.ok(), `${path}: ${await res.text()}`).toBe(true);
}

const pause = (ms: number) => new Promise((done) => setTimeout(done, ms));

test('the replay overlay shows a finished race as it stood at each moment', async ({ page }) => {
  const before = (await (await page.request.get('/active-event')).json()) as {
    event: { id: string } | null;
  };
  const made: { event?: string; pilot?: string } = {};
  const replay = await page.context().newPage();
  const replayErrors: string[] = [];
  replay.on('pageerror', (error) => replayErrors.push(String(error)));

  try {
    // ── A pilot, an event and a heat of one, on the bench ────────────────────────────────
    const pilot = await page.request.post('/pilots', { data: { callsign: ACE } });
    made.pilot = ((await pilot.json()) as { id: string }).id;
    const created = await page.request.post('/events', { data: { name: EVENT } });
    const eventId = ((await created.json()) as { id: string }).id;
    made.event = eventId;
    expect((await page.request.put('/active-event', { data: { id: eventId } })).ok()).toBe(true);
    const commands = `/events/${encodeURIComponent(eventId)}/commands`;
    await post(page, commands, {
      Record: {
        PhaseDefined: {
          def: {
            id: HEAT,
            label: HEAT_LABEL,
            kind: {
              Heat: {
                lineup: [{ adapter: 'bench', seat: 'seat-1', source: { Pilot: made.pilot } }]
              }
            },
            format: {
              name: 'Open',
              race: { end: 'Open', staging_micros: 0, first_lap: 'HoleShot' },
              completion: 'DirectorDecides'
            }
          }
        }
      }
    });

    // ── The race: a hole shot and two laps, then stopped and saved ───────────────────────
    const race = `${HEAT}#1`;
    await post(page, commands, { StartRace: { heat: HEAT } });
    for (const wait of [300, 500, 500]) {
      await pause(wait);
      await post(page, `/events/${encodeURIComponent(eventId)}/passes`, {
        adapter: 'bench',
        seat: 'seat-1',
        at: Date.now() * 1000
      });
    }
    await pause(400);
    await post(page, commands, { StopRace: { race } });
    await post(page, commands, { SaveRace: { race, needs_marshal: false } });
    const state = (await (
      await page.request.get(`/events/${encodeURIComponent(eventId)}/state`)
    ).json()) as Snapshot;
    const flown = state.races.find((candidate) => candidate.race === race)!;
    expect(flown.seats[0].laps).toHaveLength(2);
    const start = flown.started_at!;
    /** Milliseconds into the replay for a moment of the race. With no recording the replay
     *  begins five seconds before the start. */
    const at = (micros: number) => (micros - start) / 1000 + 5000;
    const [first, second] = flown.seats[0].laps.map((lap) => lap.ended_at);

    // ── The replay page, as the Director's browser opens it ──────────────────────────────
    await replay.setViewportSize({ width: 1920, height: 1080 });
    await replay.goto(
      `/#/overlay/replay/${encodeURIComponent(eventId)}/${encodeURIComponent(race)}`
    );
    await replay.waitForFunction(() => (window as ReplayWindow).gridfpvReplay !== undefined);
    expect(await replay.evaluate(() => (window as ReplayWindow).gridfpvReplay!.ready())).toBe('');
    const seek = (ms: number) =>
      replay.evaluate((to) => (window as ReplayWindow).gridfpvReplay!.seek(to), ms);
    const word = replay.getByTestId('overlay-state');
    const laps = replay.locator('[data-stat="laps"]');

    // Before the start: the countdown, and nothing flown.
    await seek(2000);
    await expect(word).toHaveText('Starting');
    await expect(replay.getByRole('timer')).toHaveText('0:03.000');
    await expect(replay.getByTestId('overlay-heat')).toHaveText(HEAT_LABEL);
    await expect(replay.getByTestId('overlay-solo')).toHaveAccessibleName(ACE);
    await expect(laps).toHaveText('0');

    // Just before the first lap ends, and just after.
    await seek(at(first) - 50);
    await expect(word).toHaveText('Live');
    await expect(laps).toHaveText('0');
    await seek(at(first) + 50);
    await expect(laps).toHaveText('1');
    const midRace = await seek(at(first) + 50);
    // After the second.
    await seek(at(second) + 50);
    await expect(laps).toHaveText('2');
    // After the stop: finished, and still. The same key means no new picture is needed.
    const stopped = await seek(at(flown.stopped_at!) + 500);
    await expect(word).toHaveText('Finished');
    await expect(laps).toHaveText('2');
    expect(await seek(at(flown.stopped_at!) + 900)).toBe(stopped);
    // And back again, to exactly the same picture.
    expect(await seek(at(first) + 50)).toBe(midRace);
    await expect(laps).toHaveText('1');

    // Bare and see-through, with no wire handle on it.
    await expect(replay.getByRole('navigation')).toHaveCount(0);
    await expect(replay.getByRole('button')).toHaveCount(0);
    const background = await replay.evaluate(() => [
      getComputedStyle(document.documentElement).backgroundColor,
      getComputedStyle(document.body).backgroundColor
    ]);
    expect(background).toEqual(['rgba(0, 0, 0, 0)', 'rgba(0, 0, 0, 0)']);
    const text = await replay.locator('body').innerText();
    for (const raw of [HEAT, '#1', 'bench', 'seat-1', eventId, made.pilot!]) {
      expect(text).not.toContain(raw);
    }

    // A race the event does not have says so, and draws nothing.
    await replay.goto(`/#/overlay/replay/${encodeURIComponent(eventId)}/nope%231`);
    await replay.reload();
    await replay.waitForFunction(() => (window as ReplayWindow).gridfpvReplay !== undefined);
    expect(await replay.evaluate(() => (window as ReplayWindow).gridfpvReplay!.ready())).toBe(
      'This race is not in the event any more.'
    );
    await expect(replay.getByRole('region', { name: 'Race overlay' })).toHaveCount(0);
    expect(replayErrors).toEqual([]);

    // ── The Video page, and the Director's refusals ──────────────────────────────────────
    await gotoPage(page, 'Video');
    await expect(page.getByTestId('no-recordings')).toHaveText(
      'This event has no recorded race yet.'
    );
    // The race was not recorded, so there is nothing to draw on.
    const refused = await page.request.post('/video/render', { data: { event: eventId, race } });
    expect(refused.status()).toBe(409);
    // Every race that needs it: none does.
    const all = await page.request.post('/video/render', { data: { event: eventId } });
    expect(await all.json()).toBe(0);
    // Recording a scene needs a scene.
    const form = page.getByRole('form', { name: 'Video settings' });
    await form.getByLabel('What records each race').selectOption('Scenes');
    await page.getByRole('button', { name: 'Save', exact: true }).click();
    await expect(form.getByRole('alert')).toContainText('Pick the scene to record');
  } finally {
    await replay.close();
    if (before.event) await page.request.put('/active-event', { data: { id: before.event.id } });
    if (made.event) await page.request.delete(`/events/${encodeURIComponent(made.event)}`);
    if (made.pilot) await page.request.delete(`/pilots/${encodeURIComponent(made.pilot)}`);
  }
});
