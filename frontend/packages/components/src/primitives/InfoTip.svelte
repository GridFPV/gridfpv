<script lang="ts">
  /**
   * InfoTip — the console's one way to say something that does not need to be on screen (#466).
   *
   * The RD's complaint was that the console explains itself in paragraphs: page leads, card
   * subtitles and standing blurbs that are read once and then sit there forever, pushing the
   * controls down the page. This is where that content goes instead — a small `?` beside the
   * heading it belongs to, which reveals the text on hover, on focus, or on click.
   *
   * ## Why a component and not a `title=` attribute
   *
   * `title=` is what the console used everywhere before this, and it has three problems that
   * matter here: it takes about a second to appear, it is invisible to keyboard users (a `title`
   * on a non-focusable element can only be reached with a mouse), and it cannot be read by a
   * screen reader in any dependable way. Moving *page help* into `title=` would make it strictly
   * less reachable than the paragraph it replaced, which is not a trade worth making.
   *
   * So: a real focusable `<button>`, the text in the DOM and bound with `aria-describedby`, shown
   * on `:hover`/`:focus-visible` and toggled by click for touch. `aria-expanded` reports which it
   * is. Nothing here traps focus or blocks the page — it is a disclosure, not a dialog.
   *
   * ## What belongs in one, and what does not
   *
   * Reference material: what a term means, how a feature behaves, what the RD could do next.
   *
   * **Not** anything the operator has to see without asking. Refusal explanations ("this is
   * disabled because…"), destructive-action warnings, results-integrity statements and
   * dangerous-state notices all stay on screen as text — see #405, where exactly that copy was
   * deliberately moved OUT of a tooltip because a disabled control with no visible reason is a
   * dead end. This component is for the paragraph nobody needs twice, not for the sentence that
   * stops a mistake.
   */
  let {
    text,
    label = 'More information',
    align = 'start'
  }: {
    /** The help text itself. Plain prose — this is reference material, not markup. */
    text: string;
    /**
     * The accessible name of the trigger. Name the THING it explains ("About channel layouts"),
     * so a screen-reader user hearing it out of context knows what they are opening.
     */
    label?: string;
    /** Which edge the bubble hangs from, so it never runs off the side of a narrow panel. */
    align?: 'start' | 'end';
  } = $props();

  let open = $state(false);
  /** Document-unique, so `aria-describedby` points at THIS tip's text and not another's. */
  const id = `gf-infotip-${Math.random().toString(36).slice(2, 10)}`;
</script>

<span class="gf-infotip" data-align={align}>
  <button
    type="button"
    class="gf-infotip-trigger"
    aria-label={label}
    aria-describedby={id}
    aria-expanded={open}
    onclick={() => (open = !open)}
    onkeydown={(e: KeyboardEvent) => {
      if (e.key === 'Escape' && open) {
        e.stopPropagation();
        open = false;
      }
    }}>?</button
  >
  <!-- Always in the DOM (never `{#if}`) so `aria-describedby` always resolves: a screen reader
       reads it on focus whether or not it is visually shown. CSS decides visibility. -->
  <span {id} class="gf-infotip-bubble" role="tooltip" data-open={open}>{text}</span>
</span>

<style>
  .gf-infotip {
    position: relative;
    display: inline-flex;
    vertical-align: middle;
    margin-left: var(--gf-space-2);
  }
  .gf-infotip-trigger {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 1.15rem;
    height: 1.15rem;
    padding: 0;
    border-radius: 50%;
    border: 1px solid var(--gf-border-strong);
    background: transparent;
    color: var(--gf-text-muted);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    line-height: 1;
    cursor: help;
  }
  .gf-infotip-trigger:hover,
  .gf-infotip-trigger[aria-expanded='true'] {
    border-color: var(--gf-accent);
    color: var(--gf-text);
  }
  .gf-infotip-trigger:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }

  .gf-infotip-bubble {
    position: absolute;
    top: calc(100% + var(--gf-space-2));
    z-index: 40;
    width: max-content;
    max-width: 22rem;
    padding: var(--gf-space-3) var(--gf-space-4);
    border-radius: var(--gf-radius-md);
    border: 1px solid var(--gf-border-strong);
    /* Opaque, deliberately: a translucent fill over arbitrary page content is unreadable, and a
       large alpha layer is what flickers under WebKitGTK (#476). */
    background: var(--gf-overlay);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-normal);
    line-height: 1.5;
    letter-spacing: normal;
    text-transform: none;
    text-align: left;
    white-space: normal;
    box-shadow: var(--gf-shadow-sm);
    /* Hidden from sight, not from the accessibility tree — `aria-describedby` still reads it. */
    opacity: 0;
    visibility: hidden;
    pointer-events: none;
  }
  .gf-infotip[data-align='start'] .gf-infotip-bubble {
    left: 0;
  }
  .gf-infotip[data-align='end'] .gf-infotip-bubble {
    right: 0;
  }
  .gf-infotip:hover .gf-infotip-bubble,
  .gf-infotip-trigger:focus-visible + .gf-infotip-bubble,
  .gf-infotip-bubble[data-open='true'] {
    opacity: 1;
    visibility: visible;
    pointer-events: auto;
  }
</style>
