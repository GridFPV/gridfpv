<script lang="ts">
  /**
   * VideoPage — have OBS record every race.
   *
   * OBS does the capturing. Grid tells it when: recording starts as a race begins its
   * countdown, stops a few seconds after the race is stopped, and the file is named after
   * the race. A discarded race's recording goes to a trash folder beside it.
   *
   * The page has the settings (where OBS is, its password, what records, the tail, the
   * trash), whether OBS is connected right now, and the runs recorded since the Director
   * started. The password is write-only: the Director never sends it back, so the field only
   * says whether one is stored.
   *
   * A race recorded clean (a scene with the camera and no overlay) can have the overlay
   * drawn onto it afterwards, with the laps as they stand. The Recordings card lists the
   * open event's recorded races with where each one's overlay stands, and the buttons that
   * draw it.
   *
   * The status has no change feed, so the page asks for it again every
   * {@link TIMER_POLL_MS} while it is open.
   */
  import {
    Badge,
    Banner,
    Button,
    Card,
    Field,
    Input,
    Select,
    StatusPill,
    toast
  } from '@gridfpv/components';
  import type { RaceId, RecordFrom, RenderWhen, VideoSettings, VideoStatus } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { raceName } from '../lib/marshal.js';
  import type { Session } from '../lib/session.svelte.js';
  import { TIMER_POLL_MS } from '../lib/timers.js';
  import {
    OVERLAY_HINTS,
    OVERLAY_LABELS,
    OVERLAY_TONES,
    RUN_STATE_LABELS,
    RUN_STATE_TONES,
    canDraw,
    fileName,
    isDrawing,
    needsDrawing,
    renderOf,
    renderWords,
    sceneChoices,
    startWords
  } from '../lib/video.js';

  let {
    session,
    live
  }: {
    session: Session;
    /** The open event's live competition, when an event is open: its recorded races. */
    live?: EventLive;
  } = $props();

  const say = (e: unknown): string => (e instanceof Error ? e.message : String(e));

  let settings = $state.raw<VideoSettings | undefined>(undefined);
  let status = $state.raw<VideoStatus | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);

  /** The form. It starts from the settings and is put back to them after a save. */
  let form = $state({
    enabled: false,
    address: '',
    password: '',
    tail: 3 as number | string,
    trash: true,
    recordFrom: 'Obs' as RecordFrom,
    cleanScene: '',
    overlayScene: '',
    renderWhen: 'Asked' as RenderWhen,
    browserPath: '',
    ffmpegPath: ''
  });
  /** Whether the password field was typed in. Untouched, the stored one is kept. */
  let passwordTyped = $state(false);
  let saving = $state(false);
  let saveError = $state<string | undefined>(undefined);

  function fill(from: VideoSettings) {
    form = {
      enabled: from.enabled,
      address: from.obs_address,
      password: '',
      tail: from.tail_seconds,
      trash: from.trash_discarded,
      recordFrom: from.record_from,
      cleanScene: from.clean_scene,
      overlayScene: from.overlay_scene,
      renderWhen: from.render_when,
      browserPath: from.browser_path,
      ffmpegPath: from.ffmpeg_path
    };
    passwordTyped = false;
  }

  async function loadSettings() {
    try {
      settings = await session.getVideoSettings();
      fill(settings);
      loadError = undefined;
    } catch (e) {
      loadError = say(e);
    }
  }
  async function loadStatus() {
    try {
      status = await session.getVideoStatus();
    } catch {
      // The settings read says what is wrong; the status keeps what it had.
    }
  }

  $effect(() => {
    void loadSettings();
    void loadStatus();
    const poll = setInterval(() => void loadStatus(), TIMER_POLL_MS);
    return () => clearInterval(poll);
  });

  const dirty = $derived(
    !!settings &&
      (form.enabled !== settings.enabled ||
        form.address.trim() !== settings.obs_address ||
        Number(form.tail) !== settings.tail_seconds ||
        form.trash !== settings.trash_discarded ||
        form.recordFrom !== settings.record_from ||
        form.cleanScene !== settings.clean_scene ||
        form.overlayScene !== settings.overlay_scene ||
        form.renderWhen !== settings.render_when ||
        form.browserPath.trim() !== settings.browser_path ||
        form.ffmpegPath.trim() !== settings.ffmpeg_path ||
        passwordTyped)
  );

  async function save(e?: Event) {
    e?.preventDefault();
    if (saving || !settings) return;
    const tail = Number(form.tail);
    if (!Number.isInteger(tail) || tail < 0) {
      saveError = 'The tail is a whole number of seconds, 0 or more.';
      return;
    }
    saving = true;
    saveError = undefined;
    try {
      const saved = await session.putVideoSettings({
        enabled: form.enabled,
        obs_address: form.address.trim(),
        // Sent only when typed: left out, the Director keeps the one it has.
        ...(passwordTyped ? { obs_password: form.password } : {}),
        tail_seconds: tail,
        trash_discarded: form.trash,
        record_from: form.recordFrom,
        clean_scene: form.cleanScene,
        overlay_scene: form.overlayScene,
        render_when: form.renderWhen,
        browser_path: form.browserPath.trim(),
        ffmpeg_path: form.ffmpegPath.trim()
      });
      if (!saved) return;
      settings = saved;
      fill(saved);
      toast.success('Video settings saved.');
      await loadStatus();
    } catch (err) {
      saveError = say(err);
    } finally {
      saving = false;
    }
  }

  /**
   * Try the address and password now, and leave the rest of the form alone. It stores just
   * those two (the Director connects from what is stored), so the scenes can be read and
   * picked before the one real Save.
   */
  async function connect() {
    if (saving || !settings) return;
    saving = true;
    saveError = undefined;
    try {
      const saved = await session.putVideoSettings({
        enabled: settings.enabled,
        obs_address: form.address.trim(),
        ...(passwordTyped ? { obs_password: form.password } : {}),
        tail_seconds: settings.tail_seconds,
        trash_discarded: settings.trash_discarded,
        record_from: settings.record_from,
        clean_scene: settings.clean_scene,
        overlay_scene: settings.overlay_scene,
        render_when: settings.render_when,
        browser_path: settings.browser_path,
        ffmpeg_path: settings.ffmpeg_path
      });
      if (!saved) return;
      // Only the stored copy moves: what else is typed in the form stays, still unsaved.
      settings = saved;
      form.address = saved.obs_address;
      form.password = '';
      passwordTyped = false;
      await loadStatus();
    } catch (err) {
      saveError = say(err);
    } finally {
      saving = false;
    }
  }

  const obs = $derived(status?.obs);
  const tools = $derived(status?.tools);

  // ── The open event's recordings ────────────────────────────────────────────
  /** The open event's races that have a recording, newest first. */
  const recorded = $derived.by(() => {
    const view = live?.view;
    if (!view || !live) return [];
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
  /** "Get ffmpeg" was just pressed, until the Director answers. */
  let askingFfmpeg = $state(false);
  async function getFfmpeg() {
    if (askingFfmpeg) return;
    askingFfmpeg = true;
    saveError = undefined;
    try {
      await session.fetchFfmpeg();
      await loadStatus();
    } catch (err) {
      saveError = say(err);
    } finally {
      askingFfmpeg = false;
    }
  }

  /** The race whose button was just pressed, or `'all'`, until the Director answers. */
  let asking = $state<RaceId | 'all' | undefined>(undefined);
  let drawError = $state<string | undefined>(undefined);

  async function draw(race?: RaceId) {
    if (!live || asking !== undefined) return;
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

<Page
  title="Video"
  help="video"
  lede="Have OBS record every race, each in its own file named after the race."
>
  {#if loadError}
    <Banner tone="danger" title="The video settings could not be read.">{loadError}</Banner>
  {/if}

  {#if settings}
    <Card title="OBS" elevation="sm">
      {#snippet actions()}
        {#if obs?.connected}
          <StatusPill status="Connected" label="Connected" size="sm" />
        {:else}
          <StatusPill status="Disconnected" label="Not connected" size="sm" />
        {/if}
      {/snippet}
      {#if obs && !obs.connected && obs.problem}
        <div data-testid="obs-problem">
          <Banner tone="warn" title="OBS is not connected.">{obs.problem}</Banner>
        </div>
      {/if}
      {#if obs?.connected}
        <p class="facts" data-testid="obs-facts">
          <span>{obs.version ?? 'OBS'}</span>
          {#if obs.record_folder}
            <span data-testid="video-folder">
              Videos are saved in <code>{obs.record_folder}</code>
            </span>
          {/if}
          {#if obs.recording}<Badge tone="danger" dot>Recording now</Badge>{/if}
        </p>
      {/if}
      <form class="form" aria-label="Video settings" onsubmit={save}>
        <label class="switch">
          <input type="checkbox" bind:checked={form.enabled} disabled={saving} />
          <span>Record every race</span>
        </label>
        <div class="row">
          <Field
            label="OBS address"
            hint="Where OBS's WebSocket server is. On this computer it is 127.0.0.1:4455."
          >
            <Input
              bind:value={form.address}
              placeholder="127.0.0.1:4455"
              aria-label="OBS address"
              autocomplete="off"
              disabled={saving}
            />
          </Field>
          <Field
            label="OBS password"
            hint={settings.has_password
              ? 'One is stored. Type here only to change it.'
              : 'From OBS: Tools, then WebSocket Server Settings.'}
          >
            <Input
              type="password"
              bind:value={
                () => form.password,
                (value) => {
                  form.password = String(value);
                  passwordTyped = true;
                }
              }
              placeholder={settings.has_password ? 'Stored' : ''}
              aria-label="OBS password"
              autocomplete="new-password"
              disabled={saving}
            />
          </Field>
        </div>
        <div>
          <Button
            size="sm"
            disabled={saving || form.address.trim() === ''}
            title="Try this address and password now. Nothing else on the page is saved."
            onclick={() => connect()}
          >
            Connect
          </Button>
        </div>
        <div class="row">
          <Field label="What records each race">
            <Select
              bind:value={form.recordFrom}
              aria-label="What records each race"
              disabled={saving}
            >
              <option value="Obs">OBS's own recording</option>
              <option value="Scenes">A scene of its own, clean</option>
            </Select>
          </Field>
          {#if form.recordFrom === 'Scenes'}
            <Field label="Clean scene" hint="The camera with no overlay on it.">
              <Select bind:value={form.cleanScene} aria-label="Clean scene" disabled={saving}>
                <option value="">Pick a scene</option>
                {#each sceneChoices(obs?.scenes, form.cleanScene) as scene (scene)}
                  <option value={scene}>{scene}</option>
                {/each}
              </Select>
            </Field>
            <Field label="Scene with the overlay" hint="Recorded as well, if you pick one.">
              <Select
                bind:value={form.overlayScene}
                aria-label="Scene with the overlay"
                disabled={saving}
              >
                <option value="">None</option>
                {#each sceneChoices(obs?.scenes, form.overlayScene) as scene (scene)}
                  <option value={scene}>{scene}</option>
                {/each}
              </Select>
            </Field>
          {/if}
        </div>
        {#if form.recordFrom === 'Scenes' && !obs?.connected}
          <p class="facts" data-testid="scenes-need-obs">
            The scenes show here once OBS is connected. Type the address and password, then press
            Connect.
          </p>
        {/if}
        {#if form.recordFrom === 'Scenes' && obs?.connected && !obs.source_record}
          <div data-testid="no-source-record">
            <Banner tone="warn" title="OBS does not have the Source Record plugin.">
              Recording a scene of its own needs it. Install Source Record in OBS, then start OBS
              again.
            </Banner>
          </div>
        {/if}
        <div class="row">
          <Field
            label="Keep recording after the stop (seconds)"
            hint="The recording ends this long after a race is stopped."
          >
            <Input
              type="number"
              inputmode="numeric"
              min="0"
              max="60"
              step="1"
              bind:value={form.tail}
              aria-label="Seconds to keep recording after the stop"
              disabled={saving}
            />
          </Field>
          <label class="switch">
            <input type="checkbox" bind:checked={form.trash} disabled={saving} />
            <span title="Into a folder called trash beside the recording. Nothing is ever deleted.">
              Move a discarded race's recording to the trash
            </span>
          </label>
        </div>
        <div class="row">
          <Field label="Draw the overlay onto clean recordings">
            <Select
              bind:value={form.renderWhen}
              aria-label="Draw the overlay onto clean recordings"
              disabled={saving}
            >
              <option value="Asked">When I ask</option>
              <option value="Always">After every race, by itself</option>
            </Select>
          </Field>
          <Field label="Browser">
            <Input
              bind:value={form.browserPath}
              placeholder={tools?.browser ?? 'Chrome, Edge or Chromium: not found'}
              title="The browser that draws the overlay. Leave this empty and Grid finds Chrome, Edge or Chromium by itself."
              aria-label="Browser"
              autocomplete="off"
              disabled={saving}
            />
          </Field>
          <Field label="ffmpeg">
            <Input
              bind:value={form.ffmpegPath}
              placeholder={tools?.ffmpeg ?? 'Not found'}
              title="ffmpeg puts the overlay onto the video. Leave this empty and Grid looks for it beside itself and on the PATH."
              aria-label="ffmpeg"
              autocomplete="off"
              disabled={saving}
            />
          </Field>
        </div>
        {#if tools && (!tools.browser || !tools.ffmpeg)}
          <p class="note" data-testid="tools-missing">
            {#if !tools.ffmpeg && !tools.browser}
              Grid cannot find a browser or ffmpeg on this computer, so it cannot draw an overlay
              onto a recording yet.
            {:else if !tools.ffmpeg}
              Grid cannot find ffmpeg on this computer, so it cannot draw an overlay onto a
              recording yet.
            {:else}
              Grid cannot find Chrome, Edge or Chromium on this computer, so it cannot draw an
              overlay onto a recording yet.
            {/if}
          </p>
        {/if}
        {#if tools && !tools.ffmpeg && (tools.can_fetch || tools.fetching)}
          <div class="fetch">
            <Button
              disabled={tools.fetching || askingFfmpeg}
              title="Grid downloads ffmpeg (about 100 MB) and keeps it in its data folder."
              onclick={getFfmpeg}
            >
              {tools.fetching ? 'Downloading ffmpeg…' : 'Get ffmpeg'}
            </Button>
          </div>
        {/if}
        {#if tools?.fetch_problem && !tools.ffmpeg}
          <Banner tone="danger" title="ffmpeg was not downloaded.">{tools.fetch_problem}</Banner>
        {/if}
        {#if saveError}
          <Banner tone="danger" ondismiss={() => (saveError = undefined)}>{saveError}</Banner>
        {/if}
        <div class="save">
          <Button
            variant="ghost"
            disabled={!dirty || saving}
            onclick={() => settings && fill(settings)}>Discard changes</Button
          >
          <Button variant="primary" disabled={!dirty || saving} onclick={() => save()}>
            {saving ? 'Saving…' : 'Save'}
          </Button>
        </div>
      </form>
    </Card>

    {#if live}
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
                <code class="file" title={entry.video.overlaid_file ?? entry.video.file}>
                  {fileName(entry.video.overlaid_file ?? entry.video.file)}
                </code>
                {#if job?.state === 'Failed' && job.note}<p class="note">{job.note}</p>{/if}
              </li>
            {/each}
          </ul>
        {/if}
      </Card>
    {/if}

    <Card title="Runs" elevation="sm">
      {#if !status || status.runs.length === 0}
        <p class="muted" data-testid="no-runs">
          {settings.enabled
            ? 'No race has been recorded since Grid started. A run shows up here as its race begins.'
            : 'Video is off. Switch it on above to have every race recorded.'}
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
                <code class="file" title={run.file}>{fileName(run.file)}</code>
              {/if}
              {#if run.note}<p class="note">{run.note}</p>{/if}
            </li>
          {/each}
        </ul>
      {/if}
    </Card>
  {:else if !loadError}
    <p class="muted" role="status">Loading…</p>
  {/if}
</Page>

<style>
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .facts {
    margin: 0 0 var(--gf-space-4);
    display: flex;
    align-items: center;
    gap: var(--gf-space-4);
    flex-wrap: wrap;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
    overflow-wrap: anywhere;
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .row {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr));
    gap: var(--gf-space-4);
    align-items: start;
  }
  .switch {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    font-weight: var(--gf-font-weight-semibold);
  }
  .fetch {
    margin-bottom: var(--space-3, 0.75rem);
  }
  .save {
    display: flex;
    justify-content: flex-end;
    gap: var(--gf-space-2);
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
