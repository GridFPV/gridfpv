/**
 * The video helpers (`lib/video.ts`): the words for a run, and what the Run page says about
 * the recording of the race on screen.
 */
import { describe, expect, it } from 'vitest';
import type { ObsStatus, RenderJob, VideoRun, VideoSettings } from '@gridfpv/types';
import {
  canDraw,
  fileName,
  isDrawing,
  needsDrawing,
  renderOf,
  renderWords,
  runVideo,
  sceneChoices,
  startWords
} from '../src/lib/video.js';
import { VIDEO_OFF, obsStatus, videoStatus } from './support.js';

const ON: VideoSettings = { ...VIDEO_OFF, enabled: true };
const CONNECTED: ObsStatus = obsStatus({ connected: true });
const run = (race: string, state: VideoRun['state'], note?: string): VideoRun => ({
  event: 'evt-1',
  race,
  name: 'Night Heat race 1',
  state,
  ...(note ? { note } : {})
});

describe('fileName', () => {
  it('drops the folders, whichever way the slashes lean', () => {
    expect(fileName('C:\\Videos\\RaceGOW 6 Track 2 race 7.mkv')).toBe(
      'RaceGOW 6 Track 2 race 7.mkv'
    );
    expect(fileName('/recordings/trash/Night race 1.mkv')).toBe('Night race 1.mkv');
    expect(fileName('plain.mkv')).toBe('plain.mkv');
  });
});

describe('startWords', () => {
  it('says how far into the recording the start tone is', () => {
    expect(startWords(4200)).toBe('4.2 s in');
    expect(startWords(0)).toBe('0.0 s in');
    expect(startWords(undefined)).toBeUndefined();
  });
});

describe('runVideo', () => {
  it('says nothing while video is off or not read yet', () => {
    expect(runVideo(undefined, 'q#1')).toEqual({ kind: 'quiet' });
    expect(runVideo({ settings: VIDEO_OFF, status: videoStatus() }, 'q#1')).toEqual({
      kind: 'quiet'
    });
  });

  it("warns before the start when OBS is not connected, with the Director's reason", () => {
    const said = runVideo(
      {
        settings: ON,
        status: videoStatus({
          obs: obsStatus({ problem: 'Could not reach OBS at 127.0.0.1:4455.' })
        })
      },
      undefined
    );
    expect(said).toEqual({
      kind: 'warning',
      title: 'OBS is not connected. A race started now is not recorded.',
      detail: 'Could not reach OBS at 127.0.0.1:4455.'
    });
    // With no reason yet it still warns.
    const waiting = runVideo({ settings: ON, status: videoStatus() }, undefined);
    expect(waiting.kind).toBe('warning');
  });

  it('warns when OBS is already recording something of its own', () => {
    const said = runVideo(
      {
        settings: ON,
        status: videoStatus({ obs: obsStatus({ connected: true, recording: true }) })
      },
      undefined
    );
    expect(said.kind).toBe('warning');
    expect(said.kind === 'warning' && said.title).toBe('OBS is already recording.');
  });

  it('is ready when OBS is connected and idle', () => {
    expect(runVideo({ settings: ON, status: videoStatus({ obs: CONNECTED }) }, undefined)).toEqual({
      kind: 'ready'
    });
  });

  it("gives the run of the race on screen, and not another race's", () => {
    const mine = run('q#2', 'Recording');
    const status = videoStatus({
      obs: obsStatus({ connected: true, recording: true }),
      runs: [mine, run('q#1', 'Saved')]
    });
    expect(runVideo({ settings: ON, status }, 'q#2')).toEqual({ kind: 'run', run: mine });
    // OBS is recording, but it is this race's own recording: no warning about it.
    expect(runVideo({ settings: ON, status }, 'q#2').kind).toBe('run');
    // A race with no run yet, while OBS records another: the warning stands.
    expect(runVideo({ settings: ON, status }, 'q#3').kind).toBe('warning');
  });
});

describe('drawing an overlay', () => {
  const job = (race: string, state: RenderJob['state'], event = 'evt-1'): RenderJob => ({
    event,
    race,
    name: 'Night Heat race 1',
    state
  });

  it('needs drawing when there is no overlay or the laps changed, and only on a clean recording', () => {
    expect(needsDrawing('Missing')).toBe(true);
    expect(needsDrawing('NeedsRender')).toBe(true);
    expect(needsDrawing('Rendered')).toBe(false);
    expect(needsDrawing('Filmed')).toBe(false);
    expect(needsDrawing('CannotRender')).toBe(false);
    expect(canDraw('CannotRender')).toBe(false);
    expect(canDraw('Filmed')).toBe(true);
  });

  it("finds a race's render by its event as well as its id", () => {
    const status = videoStatus({ renders: [job('q#1', 'Queued', 'evt-2'), job('q#1', 'Done')] });
    expect(renderOf(status, 'evt-1', 'q#1')?.state).toBe('Done');
    expect(renderOf(status, 'evt-2', 'q#1')?.state).toBe('Queued');
    expect(renderOf(status, 'evt-1', 'q#2')).toBeUndefined();
    expect(renderOf(undefined, 'evt-1', 'q#1')).toBeUndefined();
  });

  it('says how far along a render is, and nothing once it is over', () => {
    expect(renderWords(job('q#1', 'Queued'))).toBe('Waiting its turn');
    expect(renderWords(job('q#1', { Rendering: { percent: 7 } }))).toBe('Drawing 7%');
    expect(renderWords(job('q#1', 'Done'))).toBeUndefined();
    expect(renderWords(job('q#1', 'Failed'))).toBeUndefined();
    expect(renderWords(undefined)).toBeUndefined();
    expect(isDrawing(job('q#1', 'Queued'))).toBe(true);
    expect(isDrawing(job('q#1', { Rendering: { percent: 0 } }))).toBe(true);
    expect(isDrawing(job('q#1', 'Failed'))).toBe(false);
    expect(isDrawing(undefined)).toBe(false);
  });

  it('offers the chosen scene even when OBS does not list it', () => {
    expect(sceneChoices(['A', 'B'], 'B')).toEqual(['A', 'B']);
    expect(sceneChoices(['A', 'B'], 'Gone')).toEqual(['Gone', 'A', 'B']);
    expect(sceneChoices(undefined, 'Camera')).toEqual(['Camera']);
    expect(sceneChoices(undefined, '')).toEqual([]);
  });
});
