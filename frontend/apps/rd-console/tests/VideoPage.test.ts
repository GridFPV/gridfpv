/**
 * The Video page: the OBS settings (with a password that is never shown), whether OBS is
 * connected, and the runs recorded so far.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { EventView, RenderRequest, VideoSettings, VideoStatus } from '@gridfpv/types';
import VideoPage from '../src/screens/VideoPage.svelte';
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
const CONNECTED: VideoStatus = videoStatus({
  obs: obsStatus({
    connected: true,
    version: 'OBS 31.0.2 on windows',
    record_folder: 'C:\\Videos',
    scenes: ['Camera', 'Camera with overlay', 'Break'],
    source_record: true
  })
});

function open(settings: VideoSettings, status: VideoStatus, over = {}) {
  const putVideoSettingsImpl = vi.fn(async (_base: string, request: { enabled: boolean }) => ({
    ...settings,
    enabled: request.enabled
  }));
  const { session } = makeTestSession({
    noEnter: true,
    getVideoSettingsImpl: vi.fn(async () => settings),
    getVideoStatusImpl: vi.fn(async () => status),
    putVideoSettingsImpl,
    ...over
  });
  render(VideoPage, { session });
  return { putVideoSettingsImpl };
}

const form = () => screen.findByRole('form', { name: 'Video settings' });

describe('VideoPage settings', () => {
  it('shows the settings, says video is off, and has nothing to save until something changes', async () => {
    open(VIDEO_OFF, IDLE);
    const f = await form();
    expect(within(f).getByRole('checkbox', { name: 'Record every race' })).not.toBeChecked();
    expect(within(f).getByLabelText('OBS address')).toHaveValue('127.0.0.1:4455');
    expect(within(f).getByLabelText('Seconds to keep recording after the stop')).toHaveValue(3);
    // The pill is about OBS, not the switch: OBS is reached even while recording is off.
    expect(screen.getByText('Not connected')).toBeVisible();
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
    expect(screen.getByTestId('no-runs')).toHaveTextContent('Video is off.');
  });

  it('never shows the password: it says one is stored, and sends none unless one is typed', async () => {
    const { putVideoSettingsImpl } = open(ON, CONNECTED);
    const f = await form();
    const password = within(f).getByLabelText('OBS password');
    expect(password).toHaveValue('');
    expect(password).toHaveAttribute('type', 'password');
    expect(within(f).getByText('One is stored. Type here only to change it.')).toBeVisible();

    await fireEvent.input(within(f).getByLabelText('Seconds to keep recording after the stop'), {
      target: { value: '6' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(putVideoSettingsImpl).toHaveBeenCalledTimes(1));
    expect(putVideoSettingsImpl).toHaveBeenCalledWith(
      'http://d.local',
      {
        enabled: true,
        obs_address: '192.168.1.20:4455',
        tail_seconds: 6,
        trash_discarded: true,
        record_from: 'Obs',
        clean_scene: '',
        overlay_scene: '',
        render_when: 'Asked',
        browser_path: '',
        ffmpeg_path: ''
      },
      'tok'
    );
  });

  it('sends a typed password, and an emptied field clears the stored one', async () => {
    const { putVideoSettingsImpl } = open(ON, CONNECTED);
    const f = await form();
    const password = within(f).getByLabelText('OBS password');
    await fireEvent.input(password, { target: { value: 'new secret' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(putVideoSettingsImpl).toHaveBeenCalledTimes(1));
    expect(putVideoSettingsImpl.mock.calls[0][1]).toMatchObject({ obs_password: 'new secret' });

    await fireEvent.input(password, { target: { value: 'x' } });
    await fireEvent.input(password, { target: { value: '' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(putVideoSettingsImpl).toHaveBeenCalledTimes(2));
    expect(putVideoSettingsImpl.mock.calls[1][1]).toMatchObject({ obs_password: '' });
  });

  it("shows the Director's sentence when it refuses, and keeps what was typed", async () => {
    open(VIDEO_OFF, IDLE, {
      putVideoSettingsImpl: vi.fn(async () => {
        throw new Error("OBS's address looks like 192.168.1.20:4455: a host, a colon, and a port.");
      })
    });
    const f = await form();
    await fireEvent.input(within(f).getByLabelText('OBS address'), { target: { value: 'obs' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await within(f).findByRole('alert')).toHaveTextContent(/a host, a colon, and a port/);
    expect(within(f).getByLabelText('OBS address')).toHaveValue('obs');
  });

  it('refuses a tail that is not a whole number of seconds, without asking the Director', async () => {
    const { putVideoSettingsImpl } = open(VIDEO_OFF, IDLE);
    const f = await form();
    await fireEvent.input(within(f).getByLabelText('Seconds to keep recording after the stop'), {
      target: { value: '2.5' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await within(f).findByRole('alert')).toHaveTextContent(/whole number of seconds/);
    expect(putVideoSettingsImpl).not.toHaveBeenCalled();
  });
});

describe('VideoPage status', () => {
  it('says which OBS answered and where it records, once connected', async () => {
    open(ON, CONNECTED);
    await form();
    await waitFor(() => expect(screen.getByText('Connected')).toBeVisible());
    const facts = screen.getByTestId('obs-facts');
    expect(facts).toHaveTextContent('OBS 31.0.2 on windows');
    expect(facts).toHaveTextContent('C:\\Videos');
    expect(screen.getByTestId('no-runs')).toHaveTextContent(/No race has been recorded/);
  });

  it("says why OBS is not connected, in the Director's words", async () => {
    open(
      ON,
      videoStatus({
        obs: obsStatus({
          problem:
            "OBS did not take the password. Check it against OBS's WebSocket Server Settings."
        })
      })
    );
    await form();
    expect(await screen.findByTestId('obs-problem')).toHaveTextContent(
      /OBS did not take the password/
    );
    expect(screen.getByText('Not connected')).toBeVisible();
  });

  it('lists the runs by name with their state, file and note, never an id', async () => {
    open(ON, {
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
    expect(screen.getByText('Recording now')).toBeVisible();
  });
});

describe('VideoPage: recording a scene of its own', () => {
  it('offers OBS\'s scenes once "a scene of its own" is picked, and saves the choice', async () => {
    const { putVideoSettingsImpl } = open(ON, CONNECTED);
    const f = await form();
    expect(screen.queryByTestId('scenes-need-obs')).toBeNull();
    // OBS's own recording has no scenes to pick.
    expect(within(f).queryByLabelText('Clean scene')).toBeNull();
    await fireEvent.change(within(f).getByLabelText('What records each race'), {
      target: { value: 'Scenes' }
    });
    const clean = within(f).getByLabelText('Clean scene');
    expect(
      within(clean)
        .getAllByRole('option')
        .map((o) => o.textContent)
    ).toEqual(['Pick a scene', 'Camera', 'Camera with overlay', 'Break']);
    await fireEvent.change(clean, { target: { value: 'Camera' } });
    await fireEvent.change(within(f).getByLabelText('Scene with the overlay'), {
      target: { value: 'Camera with overlay' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(putVideoSettingsImpl).toHaveBeenCalledTimes(1));
    expect(putVideoSettingsImpl.mock.calls[0][1]).toMatchObject({
      record_from: 'Scenes',
      clean_scene: 'Camera',
      overlay_scene: 'Camera with overlay'
    });
  });

  it('Connect stores only the address and password, and leaves the rest of the form unsaved', async () => {
    const { putVideoSettingsImpl } = open(VIDEO_OFF, IDLE);
    const f = await form();
    await fireEvent.change(within(f).getByLabelText('What records each race'), {
      target: { value: 'Scenes' }
    });
    await fireEvent.input(within(f).getByLabelText('OBS password'), {
      target: { value: 'secret' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await waitFor(() => expect(putVideoSettingsImpl).toHaveBeenCalledTimes(1));
    // What is stored for the rest goes back as it was: no scene is needed to connect.
    expect(putVideoSettingsImpl.mock.calls[0]?.[1]).toMatchObject({
      enabled: false,
      obs_address: '127.0.0.1:4455',
      obs_password: 'secret',
      record_from: 'Obs',
      clean_scene: ''
    });
    // The choice made in the form is still there, and still waiting for Save.
    expect(within(f).getByLabelText('What records each race')).toHaveValue('Scenes');
    expect(screen.getByRole('button', { name: 'Save' })).toBeEnabled();
  });

  it('keeps a stored scene in the picker while OBS is closed', async () => {
    open({ ...ON, record_from: 'Scenes', clean_scene: 'Camera', overlay_scene: '' }, IDLE);
    const f = await form();
    expect(within(f).getByLabelText('Clean scene')).toHaveValue('Camera');
    expect(within(f).getByLabelText('Scene with the overlay')).toHaveValue('');
    expect(screen.getByTestId('scenes-need-obs')).toHaveTextContent(
      'The scenes show here once OBS is connected.'
    );
  });

  it('says so when OBS has no Source Record plugin', async () => {
    open(
      { ...ON, record_from: 'Scenes', clean_scene: 'Camera' },
      videoStatus({ obs: obsStatus({ connected: true, scenes: ['Camera'], source_record: false }) })
    );
    await form();
    expect(await screen.findByTestId('no-source-record')).toHaveTextContent(
      'OBS does not have the Source Record plugin.'
    );
  });

  it('says which program is missing for drawing overlays, and nothing when both are found', async () => {
    open(
      ON,
      videoStatus({ tools: { browser: '/usr/bin/chromium', can_fetch: false, fetching: false } })
    );
    await form();
    expect(await screen.findByTestId('tools-missing')).toHaveTextContent('cannot find ffmpeg');
    // Grid has no build of ffmpeg for this computer: there is nothing to press.
    expect(screen.queryByRole('button', { name: 'Get ffmpeg' })).toBeNull();
    cleanup();
    open(ON, CONNECTED);
    await form();
    expect(screen.queryByTestId('tools-missing')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Get ffmpeg' })).toBeNull();
  });

  it('gets ffmpeg when asked, shows the download, and says why one failed', async () => {
    const missing = { browser: '/usr/bin/chromium', can_fetch: true, fetching: false };
    const postVideoFfmpegImpl = vi.fn(async () => {});
    open(ON, videoStatus({ tools: missing }), { postVideoFfmpegImpl });
    await form();
    await fireEvent.click(await screen.findByRole('button', { name: 'Get ffmpeg' }));
    await waitFor(() => expect(postVideoFfmpegImpl).toHaveBeenCalledOnce());
    cleanup();

    open(ON, videoStatus({ tools: { ...missing, fetching: true } }));
    await form();
    expect(await screen.findByRole('button', { name: 'Downloading ffmpeg…' })).toBeDisabled();
    cleanup();

    open(
      ON,
      videoStatus({
        tools: { ...missing, fetch_problem: 'Could not download ffmpeg: no route to host' }
      })
    );
    await form();
    expect(await screen.findByText('ffmpeg was not downloaded.')).toBeVisible();
    expect(screen.getByText('Could not download ffmpeg: no route to host')).toBeVisible();
    expect(screen.getByRole('button', { name: 'Get ffmpeg' })).toBeEnabled();
  });
});

describe("VideoPage: the open event's recordings", () => {
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
    render(VideoPage, { session, live });
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

  it('has no Recordings card when no event is open', async () => {
    open(ON, IDLE);
    await form();
    expect(screen.queryByRole('list', { name: 'Recordings' })).toBeNull();
    expect(screen.queryByTestId('no-recordings')).toBeNull();
  });
});
