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
/**
 * The frame length the frame buttons start from, in seconds: a frame of 60 fps video. It is
 * only a first guess. The player learns the real length from the video itself.
 */
export const FRAME_SECONDS = 1 / 60;
/** How many times one press of a frame button looks further on before it gives up. */
const FRAME_TRIES = 12;

/**
 * Move a paused video one picture forward (`dir` 1) or back (`dir` -1), whatever its frame
 * rate is. Resolves with the time of the picture it ends on, or `undefined` when the player
 * showed nothing.
 *
 * A video's frame rate cannot be asked for, and a step of a fixed length is wrong for most
 * files: on 30 fps video, 1/60 s is half a frame, so every other press lands on the picture
 * already showing. So this looks instead. It goes on by `frame` seconds, asks which picture
 * the player then shows, and goes on again until that picture is a new one.
 *
 * - `from`: the time of the picture showing now.
 * - `frame`: how long a frame is thought to be. Too short costs a second look. Too long
 *   skips a picture, so a guess must be short.
 * - `duration`: the video's length in seconds, when it is known.
 * - `show`: move the player to a time, and resolve with the time of the picture it shows.
 *
 * Each look aims a quarter of a frame past where the picture should begin, so rounding in
 * either number cannot put it one picture out.
 */
export async function stepOneFrame(
  dir: 1 | -1,
  from: number,
  frame: number,
  duration: number,
  show: (seconds: number) => Promise<number | undefined>
): Promise<number | undefined> {
  const end = Number.isFinite(duration) ? duration : Infinity;
  let shown: number | undefined;
  for (let tries = 1; tries <= FRAME_TRIES; tries++) {
    const target = Math.min(end, Math.max(0, from + dir * tries * frame + frame / 4));
    shown = await show(target);
    if (shown === undefined || shown !== from) return shown;
    // The first or last picture: there is nothing further on to look for.
    if (target <= 0 || target >= end) return shown;
  }
  return shown;
}
