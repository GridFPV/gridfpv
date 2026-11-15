/**
 * Pure helpers for the video on the Marshal page: which of a race's videos can be played,
 * and how a moment of the video maps to a moment of the race.
 *
 * A recording keeps the Director's clock at its first frame, and a race keeps its start on
 * the same clock. So the two line up exactly, with nothing to tune: a moment `t` seconds
 * into the video is `t` minus where the start sits in the file, into the race.
 */
import type { RaceVideoView, RaceView } from '@gridfpv/types';

/** Which of a race's videos to play: the wire's own two names. */
export type WhichVideo = 'overlay' | 'recording';

/** One video the marshal can watch. */
export interface VideoChoice {
  which: WhichVideo;
  /** What the picker calls it. */
  label: string;
  /** Its overlay shows laps that have changed since. The clock on it is still right. */
  outOfDate: boolean;
  /** The file, as a path on the Director's machine. Shown so it can be found. */
  file: string;
}

/**
 * The videos of a race, the one to open on first. That is the one with the overlay in it,
 * even when its laps are out of date: the clock drawn on it is what a marshal reads. A
 * clean recording is offered second.
 */
export function videoChoices(video: RaceVideoView): VideoChoice[] {
  const choices: VideoChoice[] = [];
  const overlay = video.overlaid_file ?? video.old_overlaid_file;
  if (overlay !== undefined) {
    choices.push({
      which: 'overlay',
      label: 'With the overlay',
      outOfDate: video.overlaid_file === undefined,
      file: overlay
    });
  }
  // A recording that is not clean is itself the one with the overlay, listed above.
  if (video.clean || overlay === undefined) {
    choices.push({
      which: 'recording',
      label: video.clean ? 'Clean, no overlay' : 'The recording',
      outOfDate: false,
      file: video.file
    });
  }
  return choices;
}

/**
 * Where the race's start sits in one of its videos, in microseconds from that video's first
 * frame. The start is the tone. `undefined` for a race that never got one.
 *
 * The two videos do not begin at the same instant: a scene filmed alongside the clean one
 * starts a moment later. Each is lined up from its own first frame.
 */
export function startInVideoMicros(race: RaceView, which: WhichVideo): number | undefined {
  const start = race.started_at ?? race.tone_at;
  const video = race.video;
  if (start === undefined || video === undefined) return undefined;
  const first =
    which === 'overlay'
      ? (video.overlaid_first_frame_at ?? video.first_frame_at)
      : video.first_frame_at;
  return start - first;
}

/** A moment of the video (seconds from its first frame) as microseconds into the race. */
export function raceMicrosAt(videoSeconds: number, startInVideo: number): number {
  return Math.round(videoSeconds * 1_000_000) - startInVideo;
}

/** A moment of the race (microseconds from its start) as seconds into the video, never before its first frame. */
export function videoSecondsAt(raceMicros: number, startInVideo: number): number {
  return Math.max(0, (raceMicros + startInVideo) / 1_000_000);
}

/** A moment of the race as the marshal reads it: "12.345 s", or "-1.200 s" before the start. */
export function formatRaceMoment(raceMicros: number): string {
  const sign = raceMicros < 0 ? '-' : '';
  const totalMs = Math.round(Math.abs(raceMicros) / 1000);
  return `${sign}${Math.floor(totalMs / 1000)}.${String(totalMs % 1000).padStart(3, '0')} s`;
}

/** The speeds the player offers, slowest first. */
export const SPEEDS: readonly number[] = [0.25, 0.5, 1];
/** One step of the frame buttons, in seconds: a frame of 60 fps video. */
export const FRAME_SECONDS = 1 / 60;
