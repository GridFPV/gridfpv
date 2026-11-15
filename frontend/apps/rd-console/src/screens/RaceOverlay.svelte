<script lang="ts">
  /**
   * RaceOverlay — the race page for an OBS browser source (`#/overlay/race`).
   *
   * It follows the Director's active event and shows that event's current race, on the
   * Director's clock. The picture itself is `RacePanel.svelte`.
   *
   * With no race on, it keeps the last saved result up for a minute, then goes down to a
   * small idle mark: the brand and the event's name. With no event it shows the brand alone.
   *
   * It only reads. It sends no command, asks for no token, and plays no sound: the console
   * plays the start tone, and a second copy from the stream would be an echo.
   *
   * `theme` is the contrast for the video underneath: `'dark'` (the default) is light text
   * on a dark panel, `'light'` is dark text on a light panel.
   */
  import { untrack } from 'svelte';
  import type { EventMeta, OverlayLook } from '@gridfpv/types';
  import { EventLive, type LiveSession } from '../lib/eventLive.svelte.js';
  import { overlaySubject } from '../lib/overlay.js';
  import { logoUrls } from '../lib/overlayPlacement.js';
  import type { OverlayTheme } from '../lib/route.js';
  import type { Session } from '../lib/session.svelte.js';
  import RacePanel from './RacePanel.svelte';

  /** How often the Director is asked which event is active. */
  const ACTIVE_POLL_MS = 5000;

  let {
    session,
    theme = 'dark',
    activePollMs = ACTIVE_POLL_MS
  }: {
    /** The reads the page makes. Narrow, so a test can hand in a stand-in. */
    session: LiveSession & Pick<Session, 'readActiveEvent' | 'getOverlayLook' | 'overlayLogoUrl'>;
    theme?: OverlayTheme;
    activePollMs?: number;
  } = $props();

  // ── Following the Director's active event ──────────────────────────────────
  // A browser source sits in a scene all evening, so the event it opened with may not be the
  // one being raced. The Director is asked on a slow interval; a read that fails keeps what
  // is showing.
  let event = $state.raw<EventMeta | undefined>(undefined);
  /** Where the panel sits and which logos it has. Read with the active event, so a change
   * made on the Overlays page shows here within a few seconds. */
  let look = $state.raw<OverlayLook | undefined>(undefined);
  $effect(() => {
    let stopped = false;
    const check = async () => {
      try {
        const active = await session.readActiveEvent();
        if (stopped) return;
        if (active?.id !== event?.id || active?.name !== event?.name) event = active ?? undefined;
      } catch {
        // The Director did not answer. Keep showing what we have.
      }
      try {
        const read = await session.getOverlayLook();
        if (stopped) return;
        if (JSON.stringify(read) !== JSON.stringify(look)) look = read;
      } catch {
        // The same: keep the look we have.
      }
    };
    void untrack(check);
    const timer = setInterval(() => void check(), activePollMs);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  });

  // ── That event's live competition ──────────────────────────────────────────
  // The same store every console screen reads, so the overlay cannot disagree with them. It
  // reads the timer list once, for seat names, and does not poll it.
  let live = $state.raw<EventLive | undefined>(undefined);
  /** When this page saw the current race go away, on the Director's clock. */
  let decidedAt = $state<number | undefined>(undefined);
  let hadCurrent = false;
  const eventId = $derived(event?.id);
  $effect(() => {
    const id = eventId;
    hadCurrent = false;
    decidedAt = undefined;
    if (id === undefined) {
      live = undefined;
      return;
    }
    const store = new EventLive(session, id, { pollTimers: false });
    live = store;
    store.start();
    return () => store.stop();
  });
  $effect(() => {
    const view = live?.view;
    if (!view || !live) return;
    const has = view.current_race !== undefined;
    if (hadCurrent && !has) decidedAt = untrack(() => live?.nowMicros());
    hadCurrent = has;
  });

  // ── The clock ──────────────────────────────────────────────────────────────
  // `now` is the Director's clock. It ticks fast while a race clock is moving, and slowly
  // otherwise, which is enough to take a saved result down on time.
  let now = $state(0);
  const subject = $derived(overlaySubject(live?.view, now, decidedAt));
  const race = $derived(subject?.race);
  const moving = $derived(race?.state === 'Staging' || race?.state === 'Running');
  $effect(() => {
    const store = live;
    if (!store) return;
    // Read the clock again whenever a snapshot lands, so the offset it brought is used at once.
    void store.view;
    now = store.nowMicros();
  });
  $effect(() => {
    const store = live;
    if (!store) return;
    const timer = setInterval(() => (now = store.nowMicros()), moving ? 50 : 1000);
    return () => clearInterval(timer);
  });
  const names = $derived(live?.names);
</script>

<RacePanel
  {theme}
  {race}
  {names}
  view={live?.view}
  {now}
  eventName={event?.name}
  placement={look?.placement}
  logos={logoUrls(look, (side) => session.overlayLogoUrl(side))}
/>
