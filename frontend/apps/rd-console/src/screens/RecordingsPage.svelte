<script lang="ts">
  /**
   * RecordingsPage — the open event's recorded races.
   *
   * OBS is set up once, on the Video page in Setup. What it recorded belongs to an event,
   * so it is here, among the event's pages.
   *
   * **Recordings** lists the event's races that have a video, newest first, with where each
   * one's overlay stands. A race recorded clean (a scene with the camera and no overlay) can
   * have the overlay drawn onto it afterwards, with the laps as they stand.
   *
   * **Runs** lists what has been recorded since Grid was started, with its state as it
   * happens: recording, saved, discarded.
   *
   * The status has no change feed, so the page asks for it again every
   * {@link TIMER_POLL_MS} while it is open.
   */
  import { Badge, Banner, Button, Card } from '@gridfpv/components';
  import type { RaceId, VideoStatus } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { raceName } from '../lib/marshal.js';
  import { formatHash } from '../lib/route.js';
  import type { Session } from '../lib/session.svelte.js';
  import { TIMER_POLL_MS } from '../lib/timers.js';
  import {
    OVERLAY_HINTS,
    OVERLAY_LABELS,
    OVERLAY_TONES,
    RUN_STATE_LABELS,
    RUN_STATE_TONES,
    canDraw,
    isDrawing,
    needsDrawing,
    renderOf,
    renderWords,
    startWords
  } from '../lib/video.js';

  let {
    session,
    live
  }: {
    session: Pick<Session, 'getVideoSettings' | 'getVideoStatus' | 'renderVideo'>;
    /** The open event's live competition: its recorded races. */
    live: EventLive;
  } = $props();

  const say = (e: unknown): string => (e instanceof Error ? e.message : String(e));

  let status = $state.raw<VideoStatus | undefined>(undefined);
  /** Whether video is switched on. Unknown until the settings are read. */
  let enabled = $state<boolean | undefined>(undefined);

  async function loadStatus() {
    try {
      status = await session.getVideoStatus();
    } catch {
      // Keep what is shown. The next read may work.
    }
  }

  $effect(() => {
    void loadStatus();
    void session
      .getVideoSettings()
      .then((settings) => (enabled = settings.enabled))
      .catch(() => {});
    const poll = setInterval(() => void loadStatus(), TIMER_POLL_MS);
    return () => clearInterval(poll);
  });

  /** The open event's races that have a recording, newest first. */
  const recorded = $derived.by(() => {
    const view = live.view;
    if (!view) return [];
    const names = live.names;
    return view.races
      .flatMap((race) =>
        race.video
          ? [{ race: race.race, name: raceName(view, names, race), video: race.video }]
          : []
      )
      .reverse();
  });
  const waiting = $derived(recorded.filter((entry) => needsDrawing(entry.video.overlay)).length);

  /** The race whose button was just pressed, or `'all'`, until the Director answers. */
  let asking = $state<RaceId | 'all' | undefined>(undefined);
  let drawError = $state<string | undefined>(undefined);

  async function draw(race?: RaceId) {
    if (asking !== undefined) return;
    asking = race ?? 'all';
    drawError = undefined;
    try {
      await session.renderVideo({ event: live.eventId, ...(race === undefined ? {} : { race }) });
      await loadStatus();
    } catch (err) {
      drawError = say(err);
    } finally {
      asking = undefined;
    }
  }
</script>

<Page title="Recordings" help="video" lede="The races of this event that were recorded.">
  {#snippet actions()}
    <a class="setup" href={formatHash({ kind: 'page', page: 'video' })}>Video setup</a>
  {/snippet}
  <Card title="Recordings" elevation="sm">
    {#snippet actions()}
      {#if waiting > 0}
        <Button
          size="sm"
          variant="secondary"
          disabled={asking !== undefined}
          onclick={() => draw()}
          title="Draw the overlay for every race here that has none, or whose laps changed"
        >
          Draw all {waiting}
        </Button>
      {/if}
    {/snippet}
    {#if drawError}
      <Banner tone="danger" ondismiss={() => (drawError = undefined)}>{drawError}</Banner>
    {/if}
    {#if recorded.length === 0}
      <p class="muted" data-testid="no-recordings">This event has no recorded race yet.</p>
    {:else}
      <ul class="runs" aria-label="Recordings">
        {#each recorded as entry (entry.race)}
          {@const job = renderOf(status, live.eventId, entry.race)}
          {@const busy = isDrawing(job) || asking === entry.race}
          <li aria-label={entry.name}>
            <div class="run-head">
              <span class="run-name">{entry.name}</span>
              <span title={OVERLAY_HINTS[entry.video.overlay]}>
                <Badge tone={OVERLAY_TONES[entry.video.overlay]}>
                  {OVERLAY_LABELS[entry.video.overlay]}
                </Badge>
              </span>
              {#if renderWords(job)}
                <span class="muted" role="status">{renderWords(job)}</span>
              {/if}
              {#if canDraw(entry.video.overlay)}
                <span class="draw">
                  <Button
                    size="sm"
                    variant={needsDrawing(entry.video.overlay) ? 'primary' : 'ghost'}
                    disabled={busy || asking === 'all'}
                    onclick={() => draw(entry.race)}
                  >
                    {needsDrawing(entry.video.overlay) ? 'Draw overlay' : 'Draw again'}
                  </Button>
                </span>
              {/if}
            </div>
            <code class="file">{entry.video.overlaid_file ?? entry.video.file}</code>
            {#if job?.state === 'Failed' && job.note}<p class="note">{job.note}</p>{/if}
          </li>
        {/each}
      </ul>
    {/if}
  </Card>

  <Card title="Runs" elevation="sm">
    {#if !status || status.runs.length === 0}
      <p class="muted" data-testid="no-runs">
        {enabled
          ? 'No race has been recorded since Grid started. A run shows up here as its race begins.'
          : 'Video is off. Switch it on, on the Video page in Setup, to have every race recorded.'}
      </p>
    {:else}
      <ul class="runs" aria-label="Recorded runs">
        {#each status.runs as run (`${run.event}/${run.race}`)}
          <li aria-label={run.name}>
            <div class="run-head">
              <span class="run-name">{run.name}</span>
              <Badge tone={RUN_STATE_TONES[run.state]} dot={run.state === 'Recording'}>
                {RUN_STATE_LABELS[run.state]}
              </Badge>
              {#if startWords(run.start_at_ms)}
                <span class="muted" title="Where the start tone is in the recording">
                  Start {startWords(run.start_at_ms)}
                </span>
              {/if}
            </div>
            {#if run.file}
              <code class="file">{run.file}</code>
            {/if}
            {#if run.note}<p class="note">{run.note}</p>{/if}
          </li>
        {/each}
      </ul>
    {/if}
  </Card>
</Page>

<style>
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .setup {
    color: var(--gf-accent);
    font-size: var(--gf-font-size-sm);
  }
  .runs {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .runs li {
    display: grid;
    gap: var(--gf-space-1);
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
  }
  .run-head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .run-name {
    font-weight: var(--gf-font-weight-semibold);
  }
  .draw {
    margin-left: auto;
  }
  .file {
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
    overflow-wrap: anywhere;
  }
  .note {
    margin: 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-warn);
  }
</style>
