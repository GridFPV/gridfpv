/**
 * The Recordings page: the open event's recorded races, with where each one's overlay
 * stands, and the runs recorded since Grid started.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { EventView, RenderRequest, VideoSettings, VideoStatus } from '@gridfpv/types';
import RecordingsPage from '../src/screens/RecordingsPage.svelte';
import { eventView, heat, race, raceSeat } from './competition.js';
import { VIDEO_OFF, makeTestSession, obsStatus, openLive, videoStatus } from './support.js';

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const ON: VideoSettings = {
  ...VIDEO_OFF,
  enabled: true,
  obs_address: '192.168.1.20:4455',
  has_password: true,
  tail_seconds: 4
};
const IDLE: VideoStatus = videoStatus();

/** The page on an event with no races, for what is about the runs alone. */
async function openRuns(settings: VideoSettings, status: VideoStatus) {
  const { session } = makeTestSession({
    noEnter: true,
    getVideoSettingsImpl: vi.fn(async () => settings),
    getVideoStatusImpl: vi.fn(async () => status)
  });
  const { live } = await openLive(eventView(), {}, 'evt-9x');
  render(RecordingsPage, { session, live });
  await screen.findByRole('heading', { name: 'Runs' });
}

describe('RecordingsPage: the runs', () => {
  it('lists the runs by name with their state, file and note, never an id', async () => {
    await openRuns(ON, {
      ...videoStatus({ obs: obsStatus({ connected: true, recording: true }) }),
      runs: [
        {
          event: 'evt-9x',
          race: 'track-2-a1b2#7',
          name: 'RaceGOW 6 Track 2 race 7',
          state: 'Recording'
        },
        {
          event: 'evt-9x',
          race: 'track-2-a1b2#6',
          name: 'RaceGOW 6 Track 2 race 6',
          state: 'Saved',
          file: 'C:\\Videos\\RaceGOW 6 Track 2 race 6.mkv',
          start_at_ms: 4200
        },
        {
          event: 'evt-9x',
          race: 'track-2-a1b2#5',
          name: 'RaceGOW 6 Track 2 race 5',
          state: 'DiscardedKept',
          file: '/recordings/RaceGOW 6 Track 2 race 5.mkv',
          note: 'The recording is on another machine, so Grid could not move it to the trash.'
        }
      ]
    });
    const list = await screen.findByRole('list', { name: 'Recorded runs' });
    const rows = within(list).getAllByRole('listitem');
    expect(rows).toHaveLength(3);
    expect(rows[0]).toHaveTextContent('RaceGOW 6 Track 2 race 7');
    expect(rows[0]).toHaveTextContent('Recording');
    expect(rows[1]).toHaveTextContent('Saved');
    expect(rows[1]).toHaveTextContent('RaceGOW 6 Track 2 race 6.mkv');
    expect(rows[1]).toHaveTextContent('Start 4.2 s in');
    expect(rows[2]).toHaveTextContent('Discarded, file kept');
    expect(rows[2]).toHaveTextContent(/on another machine/);
    expect(document.body.textContent).not.toContain('track-2-a1b2');
    expect(document.body.textContent).not.toContain('evt-9x');
  });

  it('says so when nothing has been recorded, and where to switch video on when it is off', async () => {
    await openRuns(ON, IDLE);
    expect(await screen.findByTestId('no-runs')).toHaveTextContent(
      'No race has been recorded since Grid started.'
    );
    expect(screen.getByTestId('no-recordings')).toHaveTextContent(
      'This event has no recorded race yet.'
    );
    cleanup();
    await openRuns(VIDEO_OFF, IDLE);
    await waitFor(() =>
      expect(screen.getByTestId('no-runs')).toHaveTextContent(
        'Video is off. Switch it on, on the Video page in Setup'
      )
    );
  });

  it('links to the video setup, which is not on this page', async () => {
    await openRuns(ON, IDLE);
    expect(screen.getByRole('link', { name: 'Video setup' })).toHaveAttribute('href', '#/video');
    expect(screen.queryByRole('form', { name: 'Video settings' })).toBeNull();
    expect(screen.queryByLabelText('OBS address')).toBeNull();
  });
});

describe("RecordingsPage: the open event's recordings", () => {
  const HEAT = 'track-2-a1b2';
  const EVENT = 'evt-77';
  const video = (n: number, overlay: 'Missing' | 'NeedsRender' | 'Rendered' | 'CannotRender') =>
    race(HEAT, n, 'Saved', {
      seats: [raceSeat(0, 'p-1a')],
      video: {
        file: `C:\\Videos\\Night Track 2 race ${n} - Clean.mkv`,
        first_frame_at: 0,
        clean: overlay !== 'CannotRender',
        overlay,
        ...(overlay === 'Rendered'
          ? { overlaid_file: `C:\\Videos\\Night Track 2 race ${n} - Overlay (redrawn).mp4` }
          : {})
      }
    });
  const view = (): EventView =>
    eventView({
      phases: [heat(HEAT, 'Track 2', ['p-1a'])],
      races: [
        video(1, 'Rendered'),
        video(2, 'NeedsRender'),
        video(3, 'Missing'),
        video(4, 'CannotRender'),
        // Not recorded at all: not listed.
        race(HEAT, 5, 'Saved', { seats: [raceSeat(0, 'p-1a')] })
      ]
    });

  async function openWith(status: VideoStatus, over = {}) {
    const postVideoRenderImpl = vi.fn(async (_base: string, _request: RenderRequest) => 1);
    const { session } = makeTestSession({
      noEnter: true,
      getVideoSettingsImpl: vi.fn(async () => ON),
      getVideoStatusImpl: vi.fn(async () => status),
      postVideoRenderImpl,
      ...over
    });
    const { live } = await openLive(view(), {}, EVENT);
    render(RecordingsPage, { session, live });
    const list = await screen.findByRole('list', { name: 'Recordings' });
    return { postVideoRenderImpl, list };
  }

  it('lists each recorded race by name, newest first, with where its overlay stands', async () => {
    const { list } = await openWith(IDLE);
    const items = within(list).getAllByRole('listitem');
    expect(items.map((item) => item.getAttribute('aria-label'))).toEqual([
      'Track 2 · race 4',
      'Track 2 · race 3',
      'Track 2 · race 2',
      'Track 2 · race 1'
    ]);
    expect(items[0]).toHaveTextContent('Overlay is in the recording');
    expect(items[1]).toHaveTextContent('No overlay yet');
    expect(items[2]).toHaveTextContent('Laps changed');
    expect(items[3]).toHaveTextContent('Overlay drawn');
    // The finished video is the file shown once there is one.
    expect(items[3]).toHaveTextContent('Night Track 2 race 1 - Overlay (redrawn).mp4');
    expect(items[1]).toHaveTextContent('Night Track 2 race 3 - Clean.mkv');
    // A recording with the overlay baked in cannot be drawn on.
    expect(within(items[0]).queryByRole('button')).toBeNull();
    expect(within(items[1]).getByRole('button', { name: 'Draw overlay' })).toBeEnabled();
    expect(within(items[3]).getByRole('button', { name: 'Draw again' })).toBeEnabled();
    const text = list.textContent ?? '';
    for (const raw of [HEAT, '#1', EVENT, 'p-1a']) expect(text).not.toContain(raw);
  });

  it('asks the Director to draw one race, and to draw every race that needs it', async () => {
    const { postVideoRenderImpl, list } = await openWith(IDLE);
    const third = within(list).getByRole('listitem', { name: 'Track 2 · race 3' });
    await fireEvent.click(within(third).getByRole('button', { name: 'Draw overlay' }));
    await waitFor(() => expect(postVideoRenderImpl).toHaveBeenCalledTimes(1));
    expect(postVideoRenderImpl).toHaveBeenCalledWith(
      'http://d.local',
      { event: EVENT, race: `${HEAT}#3` },
      'tok'
    );
    // Two races need drawing: the one with no overlay and the one whose laps changed.
    await fireEvent.click(await screen.findByRole('button', { name: 'Draw all 2' }));
    await waitFor(() => expect(postVideoRenderImpl).toHaveBeenCalledTimes(2));
    expect(postVideoRenderImpl.mock.calls[1][1]).toEqual({ event: EVENT });
  });

  it('shows a drawing in progress, and why one failed', async () => {
    const { list } = await openWith(
      videoStatus({
        renders: [
          { event: EVENT, race: `${HEAT}#3`, name: 'x', state: { Rendering: { percent: 42 } } },
          {
            event: EVENT,
            race: `${HEAT}#2`,
            name: 'x',
            state: 'Failed',
            note: 'Grid could not find ffmpeg.'
          },
          // Another event's race with the same id is not this one.
          { event: 'evt-other', race: `${HEAT}#1`, name: 'x', state: 'Queued' }
        ]
      })
    );
    const third = within(list).getByRole('listitem', { name: 'Track 2 · race 3' });
    expect(within(third).getByRole('status')).toHaveTextContent('Drawing 42%');
    expect(within(third).getByRole('button', { name: 'Draw overlay' })).toBeDisabled();
    const second = within(list).getByRole('listitem', { name: 'Track 2 · race 2' });
    expect(second).toHaveTextContent('Grid could not find ffmpeg.');
    expect(within(second).getByRole('button', { name: 'Draw overlay' })).toBeEnabled();
    const first = within(list).getByRole('listitem', { name: 'Track 2 · race 1' });
    expect(within(first).queryByRole('status')).toBeNull();
  });

  it("shows the Director's sentence when it refuses to draw", async () => {
    const { list } = await openWith(IDLE, {
      postVideoRenderImpl: vi.fn(async () => {
        throw new Error('This race has no clean recording to draw on.');
      })
    });
    const third = within(list).getByRole('listitem', { name: 'Track 2 · race 3' });
    await fireEvent.click(within(third).getByRole('button', { name: 'Draw overlay' }));
    expect(await screen.findByText('This race has no clean recording to draw on.')).toBeVisible();
  });
});
