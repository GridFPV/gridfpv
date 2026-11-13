<script lang="ts">
  /**
   * Page — the frame every console page shares, under the nav: a title, a round `?` that
   * opens the page's help, one line saying what the page is for, the page's main buttons on
   * the right, then the page itself.
   */
  import type { Snippet } from 'svelte';
  import { formatHash } from './lib/route.js';

  let {
    title,
    help,
    lede,
    actions,
    children
  }: {
    title: string;
    /** The help page this page's `?` opens. Left out, there is no `?`. */
    help?: string;
    /** One line under the title: what the page is for. */
    lede?: string;
    /** Buttons at the right of the title. */
    actions?: Snippet;
    children: Snippet;
  } = $props();
</script>

<main class="page">
  <header class="head">
    <div class="titles">
      <h1 class="title">
        {title}
        {#if help}
          <a
            class="help"
            href={formatHash({ kind: 'help', slug: help })}
            aria-label={`Help for ${title}`}
            title="Help for this page">?</a
          >
        {/if}
      </h1>
      {#if lede}<p class="lede">{lede}</p>{/if}
    </div>
    {#if actions}<div class="actions">{@render actions()}</div>{/if}
  </header>
  {@render children()}
</main>

<style>
  .page {
    width: min(76rem, 100%);
    margin: 0 auto;
    padding: var(--gf-space-5) var(--gf-space-6) var(--gf-space-8);
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: var(--gf-space-4);
    flex-wrap: wrap;
  }
  .titles {
    margin-right: auto;
    min-width: 0;
  }
  .title {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    margin: 0;
    font-size: var(--gf-font-size-2xl);
    letter-spacing: var(--gf-tracking-tight);
    overflow-wrap: anywhere;
  }
  .help {
    display: inline-grid;
    place-items: center;
    flex-shrink: 0;
    width: 1.5rem;
    height: 1.5rem;
    border: 1px solid var(--gf-border);
    border-radius: 50%;
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-regular);
    letter-spacing: normal;
    text-decoration: none;
    color: var(--gf-text-muted);
  }
  .help:hover {
    color: var(--gf-text);
    border-color: var(--gf-text-muted);
  }
  .help:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  .lede {
    margin: var(--gf-space-1) 0 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-md);
  }
  .actions {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  @media (max-width: 40rem) {
    .page {
      padding-inline: var(--gf-space-4);
    }
  }
</style>
