<script lang="ts">
  /**
   * ReplayOverlay — the race overlay for a race that is over, shown as it stood at any
   * moment of the race's recording (`#/overlay/replay/‹event›/‹race›`).
   *
   * This is the page the Director photographs, frame by frame, to draw the overlay onto a
   * clean recording. It reads the event once and then never again: every frame of one video
   * must come from the same laps. It has no clock of its own. Whoever opened it moves it
   * through the handle it puts on the window:
   *
   * - `window.gridfpvReplay.ready()` resolves once the race is loaded and drawn. It gives an
   *   empty string, or a sentence saying why the race cannot be shown.
   * - `window.gridfpvReplay.seek(ms)` shows the moment `ms` milliseconds into the recording
   *   and resolves once that is on the page. It gives a key for what is shown: two moments
   *   with the same key look the same, so the second needs no new picture.
   *
   * The picture is `RacePanel.svelte`, the same one the live overlay draws.
   */
  import { onMount, tick } from 'svelte';
  import type { EventId, EventView, OverlayLook, RaceId } from '@gridfpv/types';
  import { buildNames, type Names } from '../lib/names.js';
  import { logoUrls } from '../lib/overlayPlacement.js';
  import { firstFrameAt, replayAt } from '../lib/replay.js';
  import type { OverlayTheme } from '../lib/route.js';
  import type { Session } from '../lib/session.svelte.js';
  import RacePanel from './RacePanel.svelte';

  /** The handle on the window. */
  interface ReplayHandle {
    ready(): Promise<string>;
    seek(ms: number): Promise<string>;
  }

  let {
    session,
    event,
    race: raceId,
    theme = 'dark'
  }: {
    /** The reads the page makes. Narrow, so a test can hand in a stand-in. */
    session: Pick<
      Session,
      | 'getEventState'
      | 'listPilots'
      | 'listTimers'
      | 'listClasses'
      | 'listEvents'
      | 'getOverlayLook'
      | 'overlayLogoUrl'
    >;
    event: EventId;
    race: RaceId;
    theme?: OverlayTheme;
  } = $props();

  let loaded = $state.raw<
    { view: EventView; names: Names; eventName?: string; look?: OverlayLook } | undefined
  >(undefined);
  let problem = $state<string | undefined>(undefined);
  /** The moment shown, in milliseconds from the recording's first frame. */
  let atMs = $state(0);
  let root = $state<HTMLDivElement>();

  /** The Director's clock at the recording's first frame. */
  const first = $derived.by(() => {
    const whole = loaded?.view.races.find((candidate) => candidate.race === raceId);
    return whole ? firstFrameAt(whole) : undefined;
  });
  const now = $derived((first ?? 0) + atMs * 1000);
  const shown = $derived(
    loaded && first !== undefined ? replayAt(loaded.view, raceId, now) : undefined
  );

  async function load(): Promise<void> {
    try {
      // The event's name leads the title, and the look places the panel. A video is still
      // worth drawing without either, so a failed read of those is not a problem.
      const [view, pilots, timers, classes, events, look] = await Promise.all([
        session.getEventState(event),
        session.listPilots(),
        session.listTimers(),
        session.listClasses(),
        session.listEvents().catch(() => []),
        session.getOverlayLook().catch(() => undefined)
      ]);
      const whole = view.races.find((candidate) => candidate.race === raceId);
      if (!whole) problem = 'This race is not in the event any more.';
      else if (firstFrameAt(whole) === undefined) problem = 'This race never started.';
      loaded = {
        view,
        names: buildNames(view.phases, pilots, timers, classes),
        eventName: events.find((candidate) => candidate.id === event)?.name,
        look
      };
    } catch (e) {
      problem = e instanceof Error ? e.message : String(e);
    }
  }

  onMount(() => {
    const loading = load();
    const handle: ReplayHandle = {
      async ready() {
        await loading;
        // Text drawn before its font has loaded would change shape a few frames in.
        await document.fonts?.ready;
        await tick();
        // The same for a logo: a frame taken before it has loaded would have a gap in it.
        await Promise.all(
          Array.from(root?.querySelectorAll('img') ?? []).map((img) =>
            img.complete ? undefined : img.decode().catch(() => undefined)
          )
        );
        await tick();
        return problem ?? '';
      },
      async seek(ms) {
        atMs = ms;
        await tick();
        const panel = root?.querySelector('.overlay');
        return `${panel?.getAttribute('data-state') ?? ''}|${root?.textContent ?? ''}`;
      }
    };
    const page = window as unknown as { gridfpvReplay?: ReplayHandle };
    page.gridfpvReplay = handle;
    return () => {
      delete page.gridfpvReplay;
    };
  });
</script>

<div bind:this={root}>
  {#if shown && loaded}
    <RacePanel
      {theme}
      race={shown.race}
      names={loaded.names}
      view={shown.view}
      {now}
      eventName={loaded.eventName}
      placement={loaded.look?.placement}
      logos={logoUrls(loaded.look, (side) => session.overlayLogoUrl(side))}
    />
  {/if}
</div>
