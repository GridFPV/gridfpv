/**
 * RaceGOW — the solo track-run loop, end to end, in real headless chromium against a real
 * Director: hub → RaceGOW page → set up a run on the Simulator → Start run → laps climb → Stop
 * run → the best 3-lap window and the submission line → Fly again → two runs in the table. Then
 * the OBS overlay page renders the same run, read-only, on a transparent document.
 *
 * This spec boots its **own** Director (a fresh registry: no pilots, no events — the first-run
 * state a pilot installing GridFPV for RaceGOW is in) with the built-in Simulator emitting six
 * laps a run, and drives real clicks. Nothing is mocked.
 */
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test } from '@playwright/test';

import { type Director, startDirector } from '../test-harness/director.js';

const here = fileURLToPath(new URL('.', import.meta.url));
const dist = resolve(here, '..', 'apps', 'rd-console', 'dist');

const TRACK = 'RaceGOW6 Track 1';
const PILOT = 'Recon';
/** Six sim laps per run: enough for a 3-lap window with room either side. */
const SIM_LAPS = 6;

let director: Director;

test.beforeAll(async () => {
  director = await startDirector({ token: false, assets: dist, simLaps: SIM_LAPS, simLapMs: 250 });
});

test.afterAll(async () => {
  await director?.stop();
});

test('a pilot sets up a track run, flies two runs, and gets a submission line', async ({
  page
}) => {
  await page.goto(director.baseUrl);

  // ── Hub → RaceGOW ────────────────────────────────────────────────────────────────────────
  await expect(page.getByRole('heading', { name: 'RaceGOW' })).toBeVisible({ timeout: 15_000 });
  await page.getByRole('heading', { name: 'RaceGOW' }).click();
  await expect(page).toHaveURL(/#\/racegow$/);
  const form = page.getByRole('form', { name: 'New track run' });
  await expect(form).toBeVisible();

  // ── The form: a fresh Director has no pilots, so the callsign field is up; the built-in
  //    Simulator is the only timer, and it carries Raceband, so a channel is offered. ─────────
  await expect(form.getByLabel('Track')).toHaveValue(TRACK);
  await form.getByLabel('Callsign').fill(PILOT);
  await expect(form.getByLabel('Timer')).toHaveValue('mock');
  await expect(form.getByLabel('Your channel')).toBeVisible();
  await form.getByRole('button', { name: 'Set up run' }).click();

  // ── The run screen, on the event the preset built ───────────────────────────────────────
  await expect(page).toHaveURL(/#\/racegow\/run$/, { timeout: 15_000 });
  await expect(page.getByRole('heading', { name: TRACK })).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText(`${PILOT} on Mock (Simulator)`)).toBeVisible();

  // The event really exists server-side, shaped by the preset: one Whoop-class round, scored by
  // best 3 consecutive laps, open-ended, with the pilot on the roster.
  const events = (await (await fetch(`${director.baseUrl}/events`)).json()) as {
    id: string;
    name: string;
    preset?: string;
    roster: string[];
    classes: string[];
    rounds?: { format: string; params: Record<string, string>; win_condition: unknown }[];
  }[];
  expect(events).toHaveLength(1);
  const event = events[0];
  expect(event.name).toBe(TRACK);
  expect(event.preset).toBe('racegow');
  expect(event.roster).toHaveLength(1);
  expect(event.classes).toEqual(['mgp-whoop']);
  expect(event.rounds).toHaveLength(1);
  expect(event.rounds![0].format).toBe('timed_qual');
  expect(event.rounds![0].params).toEqual({ rounds: '0' });
  expect(event.rounds![0].win_condition).toEqual({ BestConsecutive: { n: 3 } });

  // ── Run 1: Start run → the sim flies its laps → Stop run ─────────────────────────────────
  const run = page.getByRole('region', { name: 'Current run' });
  await run.getByRole('button', { name: 'Start run' }).click();
  // The Simulator emits laps once the heat is Running (after the fixed 1 s hold).
  const laps = run.getByRole('list', { name: "This run's laps" });
  await expect(laps.getByRole('listitem').filter({ hasText: 'Lap' })).toHaveCount(SIM_LAPS, {
    timeout: 20_000
  });
  // With ≥ 3 laps the live best-3 readout has a time.
  await expect(run.getByLabel("This run's best 3 consecutive laps")).not.toHaveText('—');
  await run.getByRole('button', { name: 'Stop run' }).click();

  // Stopped + saved: the headline is the served round-wide best, the table lists run 1 as saved,
  // and the submission line is ready with the track and the pilot.
  await expect(run.getByRole('button', { name: 'Fly again' })).toBeVisible({ timeout: 15_000 });
  await expect(page.getByLabel('Best 3 consecutive laps, all runs')).not.toHaveText('—', {
    timeout: 15_000
  });
  const table = page.getByRole('table', { name: 'Runs' });
  await expect(table.getByRole('row').filter({ hasText: 'Run 1' })).toContainText('saved');
  const submission = page.getByLabel('Submission text');
  await expect(submission).toHaveValue(new RegExp(`${TRACK} — ${PILOT}`));
  await expect(submission).toHaveValue(/Best 3 consecutive laps: \d+\.\d{3} s/);
  await expect(submission).toHaveValue(/Run 1 of 1/);

  // ── Run 2: Fly again draws the next heat and runs it ─────────────────────────────────────
  await run.getByRole('button', { name: 'Fly again' }).click();
  await expect(laps.getByRole('listitem').filter({ hasText: 'Lap' })).toHaveCount(SIM_LAPS, {
    timeout: 20_000
  });
  await run.getByRole('button', { name: 'Stop run' }).click();
  await expect(run.getByRole('button', { name: 'Fly again' })).toBeVisible({ timeout: 15_000 });
  await expect(table.getByRole('row').filter({ hasText: 'Run 2' })).toContainText('saved');
  await expect(submission).toHaveValue(/of 2/);

  // The server agrees: two Final heats on the round, and the standings carry a best-3 window.
  const heats = (await (await fetch(`${director.baseUrl}/events/${event.id}/heats`)).json()) as {
    phase: string;
  }[];
  expect(heats.map((h) => h.phase)).toEqual(['Final', 'Final']);
  const roundId = (
    (await (await fetch(`${director.baseUrl}/events`)).json()) as { rounds: { id: string }[] }[]
  )[0].rounds[0].id;
  const standings = (await (
    await fetch(`${director.baseUrl}/events/${event.id}/rounds/${roundId}/standings`)
  ).json()) as { metric: { BestConsecutive?: { n: number; micros: number | null } } }[];
  expect(standings).toHaveLength(1);
  expect(standings[0].metric.BestConsecutive?.n).toBe(3);
  expect(standings[0].metric.BestConsecutive?.micros).not.toBeNull();

  // ── The overlay: the OBS browser source follows the active run, read-only ───────────────
  await page.goto(`${director.baseUrl}/#/overlay/racegow`);
  await expect(page.getByLabel('RaceGOW overlay')).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText(`${TRACK} · ${PILOT}`)).toBeVisible({ timeout: 15_000 });
  await expect(page.getByRole('timer', { name: /Run time/ })).toBeVisible();
  await expect(page.getByLabel('Best 3 consecutive laps, all runs')).not.toHaveText('—', {
    timeout: 15_000
  });
  // The document is flagged transparent for the keyer.
  await expect(page.locator('html')).toHaveAttribute('data-overlay', 'racegow');
  await expect(page.locator('body')).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)');

  // ── Back on the RaceGOW page, the run is listed and reopens ─────────────────────────────
  await page.goto(`${director.baseUrl}/#/racegow`);
  const runs = page.getByRole('list', { name: 'Your runs' });
  await expect(runs.getByRole('listitem')).toHaveCount(1, { timeout: 15_000 });
  await runs.getByRole('button', { name: `Open ${TRACK}` }).click();
  await expect(page).toHaveURL(/#\/racegow\/run$/, { timeout: 15_000 });
  await expect(page.getByRole('heading', { name: TRACK })).toBeVisible();
});
