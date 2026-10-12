<script lang="ts">
  /**
   * EventOverlays — the in-event **Overlays** tab: where the RD sees the OBS browser-source pages
   * and gets their URLs.
   *
   * An overlay follows the Director's active event (streaming.html §2.1), so it has no setup of its
   * own — but it does need to be *findable* from inside the event, in any format, and it needs to
   * be seen before it goes in a scene. This tab lists the catalogue (`lib/overlay.ts`): for each
   * page, what it shows, a contrast picker (the URL carries the choice), the URL to paste into OBS
   * with a copy button, an "open in a new tab" link, and a live **preview** — the real overlay page
   * in a frame, over a dark backdrop standing in for the video. The preview is the same route OBS
   * loads, so what the RD sees here is what the stream gets.
   */
  import { Button, Card, toast } from '@gridfpv/components';
  import type { Session } from '../lib/session.svelte.js';
  import { OVERLAY_CATALOG, overlayUrl } from '../lib/overlay.js';
  import { formatHash, type OverlayName, type OverlayTheme } from '../lib/route.js';

  let { session }: { session: Session } = $props();

  // The contrast per overlay, chosen here so the copied URL carries it: light text on a dark panel
  // for goggle DVR, dark text on a light panel for a bright room camera (`route.ts`).
  let themes = $state<Record<OverlayName, OverlayTheme>>({ race: 'dark' });

  function urlFor(name: OverlayName): string {
    const theme = themes[name];
    return typeof location !== 'undefined'
      ? overlayUrl(location, name, theme)
      : formatHash({ kind: 'overlay', overlay: name, theme });
  }

  async function copy(text: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      toast.success('Overlay URL copied.');
    } catch {
      // The Clipboard API needs a secure context; over plain LAN HTTP it throws. The text sits in
      // a selectable box right there, so say so rather than fail silently.
      toast.info('Couldn’t copy automatically — select the URL and copy it.');
    }
  }

  const eventName = $derived(session.currentEvent?.name ?? 'this event');
</script>

<div class="overlays">
  <p class="intro">
    Overlays are pages you put on top of your video in OBS. Each one follows whatever is happening
    in <strong>{eventName}</strong> — the current heat, live — and only reads: closing OBS never
    touches the race. Add one as a <strong>Browser source</strong>, paste its URL, and make it the
    same size as your video. The background is see-through.
  </p>

  {#each OVERLAY_CATALOG as info (info.name)}
    {@const url = urlFor(info.name)}
    <Card title={info.title} elevation="sm">
      <div class="overlay">
        <div class="controls">
          <p class="muted">{info.description}</p>
          <div class="theme-pick" role="radiogroup" aria-label="{info.title} colors">
            <button
              type="button"
              class="theme-opt"
              role="radio"
              aria-checked={themes[info.name] === 'dark'}
              onclick={() => (themes[info.name] = 'dark')}
            >
              Light text on dark
            </button>
            <button
              type="button"
              class="theme-opt"
              role="radio"
              aria-checked={themes[info.name] === 'light'}
              onclick={() => (themes[info.name] = 'light')}
            >
              Dark text on light
            </button>
          </div>
          <code class="url" aria-label="{info.title} URL">{url}</code>
          <div class="row-actions">
            <Button size="sm" variant="secondary" onclick={() => copy(url)}>Copy URL</Button>
            <a class="open-link" href={url} target="_blank" rel="noopener">Open in a new tab</a>
          </div>
        </div>
        <div class="preview" aria-label="{info.title} preview">
          <!-- The real overlay route, in a frame: what OBS gets. Inert to the pointer so a stray
               click lands on the tab, not on a page that has nothing to click anyway. -->
          <iframe class="preview-frame" title="{info.title} preview" src={url} tabindex="-1"
          ></iframe>
        </div>
      </div>
    </Card>
  {/each}
</div>

<style>
  .overlays {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .intro {
    margin: 0;
    color: var(--gf-text-muted);
    max-width: 60rem;
  }
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
  }
  .overlay {
    display: grid;
    grid-template-columns: minmax(18rem, 1fr) minmax(20rem, 1.4fr);
    gap: var(--gf-space-5);
    align-items: start;
  }
  @media (max-width: 70rem) {
    .overlay {
      grid-template-columns: 1fr;
    }
  }
  .controls {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .theme-pick {
    display: inline-flex;
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
    font-weight: 600;
  }
  .url {
    display: block;
    width: 100%;
    padding: var(--gf-space-3);
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-border);
    background: var(--gf-surface-sunken);
    color: var(--gf-text);
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-xs);
    line-height: 1.5;
    user-select: all;
    overflow-wrap: anywhere;
  }
  .row-actions {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
  }
  .open-link {
    color: var(--gf-accent);
    font-size: var(--gf-font-size-sm);
  }
  /* A 16:9 frame over a dark gradient — a stand-in for the footage the panel will sit on. */
  .preview {
    position: relative;
    aspect-ratio: 16 / 9;
    border-radius: var(--gf-radius-md);
    border: 1px solid var(--gf-border);
    background: linear-gradient(135deg, #1f2a3a 0%, #0b0f16 60%, #233047 100%);
    overflow: hidden;
  }
  .preview-frame {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    border: 0;
    background: transparent;
    pointer-events: none;
  }
</style>
