<script lang="ts">
  /**
   * Brand — the GridFPV monogram + wordmark as the app's **home root** (#118).
   *
   * One shared rendering of the top-left brand button: the event workspace's sidebar and the
   * app-level directory pages (Pilots / Classes / Events / Timers) all mount this, so leaving
   * for the home hub looks and behaves identically everywhere. The markup/styles were lifted
   * verbatim from the workspace sidebar (App.svelte) when the directory pages gained the brand.
   */
  let {
    onclick,
    sub = 'RD Console'
  }: {
    /** Navigate home (the hub) — the button's only action. */
    onclick: () => void;
    /** The small caps sub-line under the wordmark. */
    sub?: string;
  } = $props();
</script>

<button type="button" class="brand" {onclick} title="Home — GridFPV hub">
  <svg class="brand-mark" viewBox="20 20 60 60" role="img" aria-label="GridFPV">
    <path
      d="M71 33 H40 Q31 33 31 42 V58 Q31 67 40 67 H60 Q69 67 69 58 V51 H53"
      fill="none"
      stroke="var(--gf-brand-500)"
      stroke-width="10"
      stroke-linecap="round"
      stroke-linejoin="round"
    />
  </svg>
  <span class="wordmark">
    <span class="name">Grid<span class="brand-fpv">FPV</span></span>
    <span class="sub">{sub}</span>
  </span>
</button>

<style>
  .brand {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-2);
    margin: calc(-1 * var(--gf-space-2));
    border: none;
    background: transparent;
    color: inherit;
    font-family: inherit;
    text-align: left;
    cursor: pointer;
    border-radius: var(--gf-radius-sm);
    transition: background var(--gf-motion-fast) var(--gf-ease-out);
  }
  .brand:hover {
    background: var(--gf-elevated);
  }
  .brand:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  .brand-mark {
    display: block;
    flex-shrink: 0;
    width: 30px;
    height: 30px;
    /* The simplified inline monogram sits directly on the dark surface — no tile. */
  }
  .wordmark {
    display: flex;
    flex-direction: column;
    line-height: 1.1;
    font-weight: var(--gf-font-weight-bold);
    font-size: var(--gf-font-size-md);
    letter-spacing: var(--gf-tracking-tight);
  }
  .wordmark .brand-fpv {
    color: var(--gf-brand-500);
  }
  .wordmark .sub {
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-medium);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }

  /* Collapse with the workspace sidebar on narrow viewports: mark only, centered (the same
     rule that lived in App.svelte when the brand was inlined there). */
  @media (max-width: 60rem) {
    .wordmark {
      display: none;
    }
    .brand {
      justify-content: center;
    }
  }
</style>
