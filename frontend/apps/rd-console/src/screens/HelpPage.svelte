<script lang="ts">
  /**
   * HelpPage — what every page does and how a race day flows through them.
   *
   * With no page named it is the index. The pages are Markdown bundled into the console
   * (`lib/help.ts`), so nothing here needs the internet. The `?` beside every page's title
   * lands on that page's help.
   */
  import { Card } from '@gridfpv/components';
  import Page from '../Page.svelte';
  import { HELP_PAGES, helpPage } from '../lib/help.js';
  import { renderMarkdown } from '../lib/markdown.js';
  import { formatHash } from '../lib/route.js';

  let { slug }: { slug?: string } = $props();

  const page = $derived(slug === undefined ? undefined : helpPage(slug));
  const html = $derived(page ? renderMarkdown(page.markdown) : '');
  const indexHref = formatHash({ kind: 'help' });
</script>

{#if page}
  <Page title={page.title}>
    <p class="back"><a href={indexHref}>All help pages</a></p>
    <Card elevation="sm">
      <!-- The Markdown is our own and the renderer escapes any HTML in it. -->
      <!-- eslint-disable-next-line svelte/no-at-html-tags -->
      <article class="doc">{@html html}</article>
    </Card>
  </Page>
{:else}
  <Page
    title="Help"
    lede="What every page does, and how a race day flows through them. Built into the console, so it works with no internet."
  >
    {#if slug !== undefined}
      <p class="back" data-testid="no-help-page">There is no help page by that name.</p>
    {/if}
    <ul class="index" aria-label="Help pages">
      {#each HELP_PAGES as entry (entry.slug)}
        <li>
          <a class="tile" href={formatHash({ kind: 'help', slug: entry.slug })}>
            <strong>{entry.title}</strong>
            <span>{entry.about}</span>
          </a>
        </li>
      {/each}
    </ul>
  </Page>
{/if}

<style>
  .back {
    margin: 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .back a {
    color: var(--gf-accent);
  }
  .index {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(17rem, 1fr));
    gap: var(--gf-space-3);
  }
  .tile {
    display: grid;
    gap: var(--gf-space-1);
    height: 100%;
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface);
    color: inherit;
    text-decoration: none;
    align-content: start;
  }
  .tile:hover {
    border-color: var(--gf-border-strong);
  }
  .tile:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }
  .tile span {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .doc :global(h1) {
    display: none;
  }
  .doc :global(h2) {
    margin: var(--gf-space-5) 0 var(--gf-space-2);
    font-size: var(--gf-font-size-lg);
  }
  .doc :global(h2:first-of-type) {
    margin-top: var(--gf-space-2);
  }
  .doc :global(h3) {
    margin: var(--gf-space-4) 0 var(--gf-space-1);
    font-size: var(--gf-font-size-md);
  }
  .doc :global(p),
  .doc :global(li) {
    line-height: 1.55;
    max-width: 72ch;
  }
  .doc :global(p) {
    margin: 0 0 var(--gf-space-3);
  }
  .doc :global(ul),
  .doc :global(ol) {
    margin: 0 0 var(--gf-space-3);
    padding-left: var(--gf-space-5);
  }
  .doc :global(li) {
    margin: var(--gf-space-1) 0;
  }
  .doc :global(a) {
    color: var(--gf-accent);
  }
  .doc :global(code) {
    font-family: var(--gf-font-mono);
    background: var(--gf-surface-sunken);
    padding: 0 4px;
    border-radius: var(--gf-radius-xs);
  }
</style>
