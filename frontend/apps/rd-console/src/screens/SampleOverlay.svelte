<script lang="ts">
  /**
   * SampleOverlay — the race overlay showing a made-up race under way
   * (`#/overlay/race/sample`), for the preview on the Overlays page.
   *
   * With no race on, the real overlay is a clock at zero, which is little to judge a corner
   * or a size by. This page draws the same panel (`RacePanel.svelte`) with one pilot a few
   * laps into a race, the clock running, and the overlay's real look: where it sits, how
   * big it is, and its logos. It reads the look once and nothing else. It never shows a real
   * race, so it is not a page to put in OBS.
   */
  import type { OverlayLook, RaceView } from '@gridfpv/types';
  import type { Names } from '../lib/names.js';
  import { logoUrls } from '../lib/overlayPlacement.js';
  import type { OverlayTheme } from '../lib/route.js';
  import type { Session } from '../lib/session.svelte.js';
  import RacePanel from './RacePanel.svelte';

  let {
    session,
    theme = 'dark'
  }: {
    session: Pick<Session, 'getOverlayLook' | 'overlayLogoUrl'>;
    theme?: OverlayTheme;
  } = $props();

  const S = 1_000_000;
  /** The laps flown so far, in seconds, after a hole shot of 2.4 s. */
  const LAPS = [9.812, 8.455, 8.102, 8.37, 8.946];
  const HOLE_SHOT = 2.4;
  /** How far into the race the picture starts: just after the last lap above. */
  const INTO = (HOLE_SHOT + LAPS.reduce((sum, lap) => sum + lap, 0) + 3.2) * S;

  /** A race that started `INTO` before the page opened. Times are on the page's own clock. */
  function sampleRace(startedAt: number): RaceView {
    let at = startedAt + HOLE_SHOT * S;
    const laps = LAPS.map((seconds, index) => {
      const started = at;
      at += seconds * S;
      return {
        crossing: index + 2,
        started_at: started,
        ended_at: at,
        micros: Math.round(seconds * S),
        restart: false,
        timer_restarted: false
      };
    });
    return {
      race: 'sample',
      heat: 'sample',
      state: 'Running',
      owner: 'Grid',
      end: { Timed: { micros: 120 * S, grace_micros: 10 * S } },
      started_at: startedAt,
      needs_marshal: false,
      locked: false,
      seats: [
        {
          adapter: 'sample',
          seat: '1',
          pilot: 'sample',
          start_at: startedAt,
          false_start: false,
          out: false,
          stints: [],
          finish: 1,
          hole_shot: { crossing: 1, at: startedAt + HOLE_SHOT * S },
          laps,
          uncounted: [],
          best_lap_micros: Math.round(Math.min(...LAPS) * S),
          total_micros: at - (startedAt + HOLE_SHOT * S)
        }
      ]
    };
  }

  /** The made-up race has one pilot and no event, so every name is said here. */
  const names: Names = {
    phase: () => 'Track 2',
    within: () => undefined,
    pilot: () => 'Your pilot',
    klass: () => 'Class',
    who: () => 'Your pilot',
    seat: () => 'Seat 1',
    seatPlain: () => 'Seat 1',
    channel: () => undefined,
    timer: () => 'Timer',
    pilotColor: () => undefined
  };

  const clock = () => Date.now() * 1000;
  const race = sampleRace(clock() - INTO);
  let now = $state(clock());
  $effect(() => {
    const timer = setInterval(() => (now = clock()), 50);
    return () => clearInterval(timer);
  });

  let look = $state.raw<OverlayLook | undefined>(undefined);
  $effect(() => {
    session.getOverlayLook().then(
      (read) => (look = read),
      () => {
        // The Director did not answer: the sample is drawn in the usual place.
      }
    );
  });
</script>

<RacePanel
  {theme}
  {race}
  {names}
  {now}
  placement={look?.placement}
  logos={logoUrls(look, (side) => session.overlayLogoUrl(side))}
  sample={{ title: 'Your event Track 2 race 3', runLength: 3 }}
/>
