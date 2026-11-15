/**
 * The video helpers for the Marshal page (`lib/marshalVideo.ts`): which videos a race has to
 * watch, and how the video's time maps to the race's.
 */
import { describe, expect, it } from 'vitest';
import type { RaceVideoView } from '@gridfpv/types';
import {
  formatRaceMoment,
  raceMicrosAt,
  startInVideoMicros,
  videoChoices,
  videoSecondsAt
} from '../src/lib/marshalVideo.js';
import { race } from './competition.js';

const S = 1_000_000;
const video = (over: Partial<RaceVideoView> = {}): RaceVideoView => ({
  file: '/v/clean.mp4',
  first_frame_at: 95 * S,
  clean: true,
  overlay: 'Missing',
  ...over
});

describe('videoChoices', () => {
  it('puts the overlay video first, and the clean one second', () => {
    expect(videoChoices(video({ overlaid_file: '/v/overlay.mp4', overlay: 'Filmed' }))).toEqual([
      { which: 'overlay', label: 'With the overlay', outOfDate: false, file: '/v/overlay.mp4' },
      { which: 'recording', label: 'Clean, no overlay', outOfDate: false, file: '/v/clean.mp4' }
    ]);
  });

  it('still offers an overlay video whose laps are out of date, marked so', () => {
    const choices = videoChoices(
      video({ old_overlaid_file: '/v/old.mp4', overlay: 'NeedsRender' })
    );
    expect(choices[0]).toEqual({
      which: 'overlay',
      label: 'With the overlay',
      outOfDate: true,
      file: '/v/old.mp4'
    });
  });

  it('a clean recording with no overlay video is the only choice', () => {
    expect(videoChoices(video()).map((choice) => choice.which)).toEqual(['recording']);
  });

  it('a recording filmed with the overlay in it is listed once, as the overlay', () => {
    const filmed = video({ clean: false, overlaid_file: '/v/clean.mp4', overlay: 'Filmed' });
    expect(videoChoices(filmed).map((choice) => choice.which)).toEqual(['overlay']);
  });
});

describe('lining the video up with the race', () => {
  it('finds where the start sits in the file', () => {
    const started = race('q', 1, 'Saved', { started_at: 100 * S, video: video() });
    expect(startInVideoMicros(started, 'recording')).toBe(5 * S);
    // Before the start is logged, the tone will do.
    const staging = race('q', 1, 'Staging', { tone_at: 99 * S, video: video() });
    expect(startInVideoMicros(staging, 'recording')).toBe(4 * S);
    expect(
      startInVideoMicros(race('q', 1, 'Saved', { started_at: 100 * S }), 'recording')
    ).toBeUndefined();
    expect(
      startInVideoMicros(race('q', 1, 'Saved', { video: video() }), 'overlay')
    ).toBeUndefined();
  });

  it('lines each video up from its own first frame', () => {
    // The overlay scene began 0.13 s after the clean one, so the start is that much earlier
    // in its file.
    const both = race('q', 1, 'Saved', {
      started_at: 100 * S,
      video: video({ overlaid_file: '/v/overlay.mp4', overlaid_first_frame_at: 95.13 * S })
    });
    expect(startInVideoMicros(both, 'recording')).toBe(5 * S);
    expect(startInVideoMicros(both, 'overlay')).toBe(4.87 * S);
    // A recording from before that was kept: the one first frame stands for both.
    const old = race('q', 1, 'Saved', {
      started_at: 100 * S,
      video: video({ overlaid_file: '/v/overlay.mp4' })
    });
    expect(startInVideoMicros(old, 'overlay')).toBe(5 * S);
  });

  it('maps a moment of the video to the race and back', () => {
    expect(raceMicrosAt(15.5, 5 * S)).toBe(10.5 * S);
    expect(raceMicrosAt(2, 5 * S)).toBe(-3 * S);
    expect(videoSecondsAt(10.5 * S, 5 * S)).toBe(15.5);
    // Never before the file's first frame.
    expect(videoSecondsAt(-9 * S, 5 * S)).toBe(0);
  });

  it('writes a moment of the race to the millisecond', () => {
    expect(formatRaceMoment(10.5 * S)).toBe('10.500 s');
    expect(formatRaceMoment(0)).toBe('0.000 s');
    expect(formatRaceMoment(-1.2 * S)).toBe('-1.200 s');
  });
});
