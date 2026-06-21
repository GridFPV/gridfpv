<script lang="ts">
  /**
   * Breadcrumbs — the app-level trail for the two-level IA (#118).
   *
   * The console is now a **home hub of pages** (Pilots / Events / Timers) plus the in-event
   * workspace. This slim trail sits at the top of every app-level page (and the workspace's
   * context bar) so the RD always knows where they are and can get **Home** from anywhere:
   *
   *   Home › Timers
   *   Home › Pilots
   *   Home › Events            (the picker)
   *   Home › Events › Friday Night Series   (the workspace, event name as the leaf)
   *
   * `Home` is always a button (the hub root); intermediate crumbs may be buttons (navigable)
   * or plain text; the **current** crumb (last) is rendered as plain, aria-current text. The
   * caller passes the trail and each crumb's optional `onclick`.
   */
  type Crumb = { label: string; onclick?: () => void };

  let { crumbs }: { crumbs: Crumb[] } = $props();
</script>

<nav class="crumbs" aria-label="Breadcrumb">
  <ol>
    {#each crumbs as crumb, i (i)}
      <li>
        {#if i > 0}
          <span class="sep" aria-hidden="true">›</span>
        {/if}
        {#if crumb.onclick && i < crumbs.length - 1}
          <button type="button" class="crumb link" onclick={crumb.onclick}>{crumb.label}</button>
        {:else}
          <span class="crumb current" aria-current={i === crumbs.length - 1 ? 'page' : undefined}>
            {crumb.label}
          </span>
        {/if}
      </li>
    {/each}
  </ol>
</nav>

<style>
  .crumbs ol {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .crumbs li {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    min-width: 0;
  }
  .sep {
    color: var(--gf-text-faint);
    font-size: var(--gf-font-size-md);
    line-height: 1;
  }
  .crumb {
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-medium);
    letter-spacing: var(--gf-tracking-tight);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 18rem;
  }
  .crumb.link {
    border: none;
    background: transparent;
    color: var(--gf-text-muted);
    cursor: pointer;
    padding: 0;
    transition: color var(--gf-motion-fast) var(--gf-ease-out);
  }
  .crumb.link:hover {
    color: var(--gf-accent);
  }
  .crumb.link:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
    border-radius: var(--gf-radius-xs);
  }
  .crumb.current {
    color: var(--gf-text);
    font-weight: var(--gf-font-weight-semibold);
  }
</style>
