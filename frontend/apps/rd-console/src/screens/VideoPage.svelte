<script lang="ts">
  /**
   * VideoPage — have OBS record every race.
   *
   * OBS does the capturing. Grid tells it when: recording starts as a race begins its
   * countdown, stops a few seconds after the race is stopped, and the file is named after
   * the race. A discarded race's recording goes to a trash folder beside it.
   *
   * The page has the settings (where OBS is, its password, what records, the tail, the
   * trash) and whether OBS is connected right now. The password is write-only: the Director
   * never sends it back, so the field only says whether one is stored.
   *
   * This is set up once. What was recorded belongs to an event, and is on the event's
   * **Recordings** page (`RecordingsPage`).
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
  import type { RecordFrom, RenderWhen, SaveIn, VideoSettings, VideoStatus } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import type { Session } from '../lib/session.svelte.js';
  import { TIMER_POLL_MS } from '../lib/timers.js';
  import { sceneChoices } from '../lib/video.js';

  let { session }: { session: Session } = $props();

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
    saveIn: 'Grid' as SaveIn,
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
      saveIn: from.save_in,
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
        form.saveIn !== settings.save_in ||
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
        ffmpeg_path: form.ffmpegPath.trim(),
        save_in: form.saveIn
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
        ffmpeg_path: settings.ffmpeg_path,
        save_in: settings.save_in
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
          <Field label="Where videos are saved">
            <Select bind:value={form.saveIn} aria-label="Where videos are saved" disabled={saving}>
              <option value="Grid">Grid's videos folder</option>
              <option value="Obs">OBS's own recording folder</option>
            </Select>
          </Field>
        </div>
        <p class="facts" data-testid="video-folder">
          {#if form.saveIn === 'Grid'}
            {#if settings.videos_folder}
              <span>Videos are saved in <code>{settings.videos_folder}</code></span>
            {/if}
            <span>Pick OBS's own folder if OBS runs on another computer.</span>
          {:else if obs?.record_folder}
            <span>Videos are saved in <code>{obs.record_folder}</code></span>
          {:else}
            <span>Videos are saved wherever OBS records to.</span>
          {/if}
        </p>
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
</style>
