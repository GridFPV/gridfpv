/**
 * Pure helpers for video: the words for a run's state, and what the Run page says about the
 * recording of the race on screen.
 */
import type {
  EventId,
  RaceId,
  RenderJob,
  VideoOverlay,
  VideoRun,
  VideoRunState,
  VideoSettings,
  VideoStatus
} from '@gridfpv/types';

/** The words for where a run's recording stands. */
export const RUN_STATE_LABELS: Record<VideoRunState, string> = {
  Recording: 'Recording',
  Finishing: 'Finishing',
  WaitingForSave: 'Waiting for the save',
  Saved: 'Saved',
  Trashed: 'In the trash',
  DiscardedKept: 'Discarded, file kept',
  Failed: 'Not recorded'
};

/** The badge tone for each state. */
export const RUN_STATE_TONES: Record<
  VideoRunState,
  'danger' | 'warn' | 'info' | 'success' | 'neutral'
> = {
  Recording: 'danger',
  Finishing: 'warn',
  WaitingForSave: 'info',
  Saved: 'success',
  Trashed: 'neutral',
  DiscardedKept: 'neutral',
  Failed: 'warn'
};

/** The words for where a race's overlaid video stands. */
export const OVERLAY_LABELS: Record<VideoOverlay, string> = {
  Filmed: 'Overlay filmed',
  Rendered: 'Overlay drawn',
  Missing: 'No overlay yet',
  NeedsRender: 'Laps changed',
  CannotRender: 'Overlay is in the recording'
};

/** The badge tone for each. */
export const OVERLAY_TONES: Record<VideoOverlay, 'warn' | 'info' | 'success' | 'neutral'> = {
  Filmed: 'success',
  Rendered: 'success',
  Missing: 'info',
  NeedsRender: 'warn',
  CannotRender: 'neutral'
};

/** What each one means, for a tooltip. */
export const OVERLAY_HINTS: Record<VideoOverlay, string> = {
  Filmed: 'OBS filmed this race with the overlay on, and the laps have not changed since.',
  Rendered: 'The overlay was drawn onto the clean recording, with the laps as they are now.',
  Missing: 'There is a clean recording. The overlay has not been drawn onto it yet.',
  NeedsRender:
    'The laps have changed since the video with the overlay was made. Draw it again to match.',
  CannotRender:
    'This race was recorded with whatever was on screen, so there is no clean video to draw on.'
};

/** Whether a race's overlay is waiting to be drawn. */
export function needsDrawing(overlay: VideoOverlay): boolean {
  return overlay === 'Missing' || overlay === 'NeedsRender';
}

/** Whether the overlay can be drawn onto a race's recording at all. */
export function canDraw(overlay: VideoOverlay): boolean {
  return overlay !== 'CannotRender';
}

/** The render asked for a race, if one was since the Director started. */
export function renderOf(
  status: VideoStatus | undefined,
  event: EventId,
  race: RaceId
): RenderJob | undefined {
  return status?.renders.find((job) => job.event === event && job.race === race);
}

/** Whether a render is waiting or being drawn. */
export function isDrawing(job: RenderJob | undefined): boolean {
  return job !== undefined && (job.state === 'Queued' || typeof job.state === 'object');
}

/** A render's progress in words, or `undefined` when there is nothing in hand to say. */
export function renderWords(job: RenderJob | undefined): string | undefined {
  if (!job) return undefined;
  if (job.state === 'Queued') return 'Waiting its turn';
  if (typeof job.state === 'object') return `Drawing ${job.state.Rendering.percent}%`;
  return undefined;
}

/**
 * The scenes a picker offers: OBS's own list, and the chosen one even when OBS does not
 * list it (OBS may be closed, or the scene renamed), so a stored choice is never silently
 * shown as another.
 */
export function sceneChoices(scenes: readonly string[] | undefined, chosen: string): string[] {
  const list = [...(scenes ?? [])];
  if (chosen !== '' && !list.includes(chosen)) list.unshift(chosen);
  return list;
}

/** A file's name without its folders, whichever way the slashes lean. */
export function fileName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/** Where the start tone is in a recording: "4.2 s in". */
export function startWords(ms: number | undefined): string | undefined {
  if (ms === undefined) return undefined;
  return `${(ms / 1000).toFixed(1)} s in`;
}

/** What the Run page shows about video for the race on screen. */
export type RunVideo =
  /** Video is off, or nothing is known yet: say nothing. */
  | { kind: 'quiet' }
  /** The race will not be recorded unless something is fixed. */
  | { kind: 'warning'; title: string; detail: string }
  /** OBS is ready and the next race will be recorded. */
  | { kind: 'ready' }
  /** This race's recording, as it stands. */
  | { kind: 'run'; run: VideoRun };

/**
 * What to say on the Run page. `race` is the race on screen (counting down, running, or
 * stopped), if there is one.
 *
 * The point of it is that a pilot never finds out afterwards that a run was not filmed: if
 * video is switched on and OBS is not there, the page says so before Start is pressed.
 */
export function runVideo(
  video: { settings: VideoSettings; status: VideoStatus } | undefined,
  race: RaceId | undefined
): RunVideo {
  if (!video || !video.settings.enabled) return { kind: 'quiet' };
  const run = race === undefined ? undefined : video.status.runs.find((r) => r.race === race);
  if (run) return { kind: 'run', run };
  const obs = video.status.obs;
  if (!obs.connected) {
    return {
      kind: 'warning',
      title: 'OBS is not connected. A race started now is not recorded.',
      detail: obs.problem ?? 'The Director is trying to reach OBS.'
    };
  }
  if (obs.recording) {
    return {
      kind: 'warning',
      title: 'OBS is already recording.',
      detail:
        'Grid leaves a recording it did not start alone, so a race started now gets no file of its own. Stop the recording in OBS to have each race recorded by itself.'
    };
  }
  return { kind: 'ready' };
}
