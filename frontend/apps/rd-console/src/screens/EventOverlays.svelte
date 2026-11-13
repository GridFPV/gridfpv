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
   */
  import { Button, toast } from '@gridfpv/components';
  import { OVERLAY_CATALOG, overlayUrl } from '../lib/overlay.js';
  import type { OverlayName, OverlayTheme } from '../lib/route.js';

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
          <!-- The real overlay route, as OBS loads it. Nothing in it can be clicked. -->
          <iframe
            class="preview-frame"
            title="{info.title} overlay preview"
            src={url}
            tabindex="-1"
            width={SOURCE_WIDTH}
            height={SOURCE_HEIGHT}
            style:transform="scale({previewWidth / SOURCE_WIDTH})"
          ></iframe>
        </div>
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
