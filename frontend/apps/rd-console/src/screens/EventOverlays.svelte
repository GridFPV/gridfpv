<script lang="ts">
  /**
   * EventOverlays — the Overlays card on the event page: where the director finds the pages
   * that go on top of video in OBS.
   *
   * For each overlay it says what the page shows and gives its URL, with a choice of
   * contrast (the URL carries it), a copy button and a link that opens it in a new tab. The
   * preview is the same page in a frame, drawn at 1920x1080 and scaled down, over a dark or
   * light backdrop that stands in for the video. It is opened on demand, because it is a
   * second live page.
   *
   * Under that is where the race overlay sits: the corner, how far in from it, the size and
   * how solid the background is, and the two logos of its title line. There is one such
   * look for the whole Director. Each change is saved as it is made, and the live page, the
   * replay and every redrawn video follow it.
   */
  import { Button, Input, toast } from '@gridfpv/components';
  import type { OverlayLook, OverlayPlacement } from '@gridfpv/types';
  import { OVERLAY_CATALOG, overlayUrl } from '../lib/overlay.js';
  import {
    CORNERS,
    CORNER_LABELS,
    OPACITY_MAX,
    OPACITY_MIN,
    SCALE_MAX,
    SCALE_MIN,
    logoUrls,
    placementProblem
  } from '../lib/overlayPlacement.js';
  import type { OverlayName, OverlayTheme } from '../lib/route.js';
  import type { Session } from '../lib/session.svelte.js';

  let {
    session
  }: {
    /** The reads and writes the card makes. Narrow, so a test can hand in a stand-in. */
    session: Pick<
      Session,
      | 'getOverlayLook'
      | 'putOverlayLook'
      | 'overlayLogoUrl'
      | 'putOverlayLogo'
      | 'deleteOverlayLogo'
    >;
  } = $props();

  /** The size the preview's page is drawn at before it is scaled to fit. */
  const SOURCE_WIDTH = 1920;
  const SOURCE_HEIGHT = 1080;

  const THEMES: readonly { theme: OverlayTheme; label: string }[] = [
    { theme: 'dark', label: 'Light text, for dark video' },
    { theme: 'light', label: 'Dark text, for bright video' }
  ];

  let themes = $state<Record<OverlayName, OverlayTheme>>({ race: 'dark' });
  let previewing = $state<Record<OverlayName, boolean>>({ race: false });
  /** The preview box's width on screen, which sets the scale. */
  let previewWidth = $state(0);

  // ── Where the race overlay sits, and its logos ─────────────────────────────
  const say = (e: unknown): string => (e instanceof Error ? e.message : String(e));
  let look = $state.raw<OverlayLook | undefined>(undefined);
  let lookError = $state<string | undefined>(undefined);
  let placing = $state(false);
  /** Counts the saves, so the preview and the logo pictures are fetched again after one. */
  let saves = $state(0);
  $effect(() => {
    session.getOverlayLook().then(
      (read) => (look = read),
      (e) => (lookError = say(e))
    );
  });

  async function change(run: () => Promise<OverlayLook | undefined>): Promise<void> {
    if (placing) return;
    placing = true;
    lookError = undefined;
    try {
      const saved = await run();
      if (saved) {
        look = saved;
        saves += 1;
      }
    } catch (e) {
      lookError = say(e);
    } finally {
      placing = false;
    }
  }

  function place(fields: Partial<OverlayPlacement>): void {
    if (!look) return;
    const next = { ...look.placement, ...fields };
    const problem = placementProblem(next);
    if (problem) {
      lookError = problem;
      return;
    }
    void change(() => session.putOverlayLook(next));
  }
  const num = (e: Event) => Number((e.currentTarget as HTMLInputElement).value);

  const SIDES = [
    { side: 'left', label: 'Left logo' },
    { side: 'right', label: 'Right logo' }
  ] as const;
  let pickers = $state<Record<'left' | 'right', HTMLInputElement | undefined>>({
    left: undefined,
    right: undefined
  });
  function chooseLogo(side: 'left' | 'right', e: Event): void {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = '';
    if (file) void change(() => session.putOverlayLogo(side, file));
  }
  const logos = $derived(logoUrls(look, (side) => session.overlayLogoUrl(side), saves));

  async function copy(url: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(url);
      toast.success('Overlay URL copied.');
    } catch {
      // The clipboard needs a secure page; over plain LAN HTTP it is refused. The URL is
      // right there to select.
      toast.info('Could not copy by itself. Select the URL and copy it.');
    }
  }
</script>

<div class="overlays">
  <p class="muted">
    An overlay is a page you put on top of your video in OBS. Add it as a Browser source at
    1920x1080 and paste its URL. It follows the event the Director has open, and it only reads.
  </p>

  {#each OVERLAY_CATALOG as info (info.name)}
    {@const url = overlayUrl(location, info.name, themes[info.name])}
    <section class="overlay" aria-label="{info.title} overlay">
      <h3 class="title">{info.title}</h3>
      <p class="muted">{info.description}</p>
      <div class="theme-pick" role="radiogroup" aria-label="{info.title} overlay contrast">
        {#each THEMES as choice (choice.theme)}
          <button
            type="button"
            class="theme-opt"
            role="radio"
            aria-checked={themes[info.name] === choice.theme}
            onclick={() => (themes[info.name] = choice.theme)}
          >
            {choice.label}
          </button>
        {/each}
      </div>
      <code class="url" aria-label="{info.title} overlay URL">{url}</code>
      <div class="row-actions">
        <Button size="sm" variant="secondary" onclick={() => copy(url)}>Copy URL</Button>
        <a class="open-link" href={url} target="_blank" rel="noopener">Open in a new tab</a>
        <Button
          size="sm"
          variant="ghost"
          aria-expanded={previewing[info.name]}
          onclick={() => (previewing[info.name] = !previewing[info.name])}
        >
          {previewing[info.name] ? 'Hide preview' : 'Show preview'}
        </Button>
      </div>
      {#if previewing[info.name]}
        <div
          class="preview"
          data-backdrop={themes[info.name]}
          aria-label="{info.title} overlay preview"
          bind:clientWidth={previewWidth}
        >
          <!-- The real overlay route, as OBS loads it. Nothing in it can be clicked. It is
               loaded again after each change to the look, so the change shows at once. -->
          {#key saves}
            <iframe
              class="preview-frame"
              title="{info.title} overlay preview"
              src={url}
              tabindex="-1"
              width={SOURCE_WIDTH}
              height={SOURCE_HEIGHT}
              style:transform="scale({previewWidth / SOURCE_WIDTH})"
            ></iframe>
          {/key}
        </div>
      {/if}
      {#if info.name === 'race' && look}
        {@const placement = look.placement}
        <fieldset class="placement" aria-label="Placement">
          <legend>Placement: the same live, in the replay and in every redrawn video</legend>
          <div class="theme-pick" role="radiogroup" aria-label="Corner">
            {#each CORNERS as corner (corner)}
              <button
                type="button"
                class="theme-opt"
                role="radio"
                aria-checked={placement.corner === corner}
                disabled={placing}
                onclick={() => place({ corner })}
              >
                {CORNER_LABELS[corner]}
              </button>
            {/each}
          </div>
          <div class="placerow">
            <label class="field">
              <span>In from the side (px)</span>
              <Input
                type="number"
                min="0"
                max="4000"
                value={String(placement.x)}
                aria-label="In from the side"
                disabled={placing}
                onchange={(e: Event) => place({ x: num(e) })}
              />
            </label>
            <label class="field">
              <span>In from the top or bottom (px)</span>
              <Input
                type="number"
                min="0"
                max="4000"
                value={String(placement.y)}
                aria-label="In from the top or bottom"
                disabled={placing}
                onchange={(e: Event) => place({ y: num(e) })}
              />
            </label>
          </div>
          <div class="placerow">
            <label class="field">
              <span>Size (%)</span>
              <Input
                type="number"
                min={String(SCALE_MIN)}
                max={String(SCALE_MAX)}
                step="5"
                value={String(placement.scale)}
                aria-label="Size"
                disabled={placing}
                onchange={(e: Event) => place({ scale: num(e) })}
              />
            </label>
            <input
              class="slider"
              type="range"
              min={SCALE_MIN}
              max={SCALE_MAX}
              step="5"
              value={placement.scale}
              aria-label="Size slider"
              disabled={placing}
              onchange={(e) => place({ scale: num(e) })}
            />
          </div>
          <div class="placerow">
            <label class="field">
              <span>Background (%)</span>
              <Input
                type="number"
                min={String(OPACITY_MIN)}
                max={String(OPACITY_MAX)}
                step="5"
                value={String(placement.opacity)}
                aria-label="Background"
                disabled={placing}
                onchange={(e: Event) => place({ opacity: num(e) })}
              />
            </label>
            <input
              class="slider"
              type="range"
              min={OPACITY_MIN}
              max={OPACITY_MAX}
              step="5"
              value={placement.opacity}
              aria-label="Background slider"
              disabled={placing}
              onchange={(e) => place({ opacity: num(e) })}
            />
          </div>
        </fieldset>
        <fieldset class="placement" aria-label="Logos">
          <legend>Logos: one for each end of the title line</legend>
          <div class="placerow">
            {#each SIDES as { side, label } (side)}
              <div class="logo-slot">
                <span class="logo-name">{label}</span>
                {#if logos[side]}
                  <img class="logo-thumb" src={logos[side]} alt="{label} now" />
                {:else}
                  <span class="muted" data-testid="no-logo-{side}">
                    {side === 'left' ? 'None. The GridFPV name shows.' : 'None.'}
                  </span>
                {/if}
                <input
                  bind:this={pickers[side]}
                  class="picker"
                  type="file"
                  accept="image/png,image/jpeg,image/webp,image/svg+xml"
                  aria-label="{label} picture"
                  onchange={(e) => chooseLogo(side, e)}
                />
                <div class="row-actions">
                  <Button
                    size="sm"
                    variant="secondary"
                    disabled={placing}
                    onclick={() => pickers[side]?.click()}
                  >
                    Choose a picture
                  </Button>
                  {#if logos[side]}
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={placing}
                      aria-label="Remove the {label.toLowerCase()}"
                      onclick={() => change(() => session.deleteOverlayLogo(side))}
                    >
                      Remove
                    </Button>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
        </fieldset>
      {/if}
      {#if info.name === 'race' && lookError}
        <p class="problem" role="alert">{lookError}</p>
      {/if}
    </section>
  {/each}
</div>

<style>
  .overlays,
  .overlay {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .title {
    margin: 0;
    font-size: var(--gf-font-size-md);
  }
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .theme-pick {
    display: inline-flex;
    flex-wrap: wrap;
    gap: 2px;
    padding: 2px;
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-border);
    background: var(--gf-surface-sunken);
    align-self: flex-start;
  }
  .theme-opt {
    appearance: none;
    border: 0;
    border-radius: calc(var(--gf-radius-sm) - 2px);
    padding: var(--gf-space-1) var(--gf-space-3);
    background: transparent;
    color: var(--gf-text-muted);
    font: inherit;
    font-size: var(--gf-font-size-sm);
    cursor: pointer;
  }
  .theme-opt[aria-checked='true'] {
    background: var(--gf-elevated);
    color: var(--gf-text);
    font-weight: var(--gf-font-weight-semibold);
  }
  .theme-opt:focus-visible {
    outline: 2px solid var(--gf-accent);
    outline-offset: 1px;
  }
  .url {
    display: block;
    padding: var(--gf-space-2) var(--gf-space-3);
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-border);
    background: var(--gf-surface-sunken);
    color: var(--gf-text);
    font-size: var(--gf-font-size-xs);
    user-select: all;
    overflow-wrap: anywhere;
  }
  .row-actions {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--gf-space-3);
  }
  .open-link {
    color: var(--gf-accent);
    font-size: var(--gf-font-size-sm);
  }
  /* A 16:9 box over a backdrop that stands in for the video: dark for the light text, bright
   * for the dark text. */
  .preview {
    position: relative;
    aspect-ratio: 16 / 9;
    border-radius: var(--gf-radius-md);
    border: 1px solid var(--gf-border);
    background: linear-gradient(135deg, #1f2a3a 0%, #0b0f16 60%, #233047 100%);
    overflow: hidden;
  }
  .preview[data-backdrop='light'] {
    background: linear-gradient(135deg, #dfe9f2 0%, #f6f1e4 60%, #c9dcea 100%);
  }
  .placement {
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    padding: var(--gf-space-2) var(--gf-space-3) var(--gf-space-3);
    margin: 0;
    display: grid;
    gap: var(--gf-space-3);
  }
  .placement legend {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
    padding: 0 var(--gf-space-1);
  }
  .placerow {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-3);
    align-items: end;
  }
  .placerow .field {
    display: grid;
    gap: var(--gf-space-1);
    min-width: 9rem;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .slider {
    flex: 1;
    min-width: 10rem;
    accent-color: var(--gf-accent);
  }
  .logo-slot {
    display: grid;
    gap: var(--gf-space-2);
    min-width: 14rem;
    align-content: start;
  }
  .logo-name {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  /* On a mid grey, so a dark mark and a light one both show. */
  .logo-thumb {
    height: 44px;
    width: auto;
    max-width: 14rem;
    object-fit: contain;
    justify-self: start;
    padding: var(--gf-space-1);
    border-radius: var(--gf-radius-sm);
    background: #4f4f4f;
  }
  .picker {
    display: none;
  }
  .problem {
    margin: 0;
    color: var(--gf-danger);
    font-size: var(--gf-font-size-sm);
  }
  .preview-frame {
    position: absolute;
    top: 0;
    left: 0;
    border: 0;
    background: transparent;
    transform-origin: 0 0;
    pointer-events: none;
  }
</style>
