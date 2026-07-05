<script lang="ts">
  /**
   * CountrySelect — a searchable country picker for the pilot directory (issue #74).
   *
   * Binds a single `value`: the stored **ISO 3166-1 alpha-2 code** (uppercase), or `''` for none.
   * The trigger shows the current country's **SVG flag + name** (or a placeholder); clicking it
   * opens a search box + a filtered list. The list is **United States pinned at the top**, then
   * every other country alphabetically by name (the bundled {@link COUNTRIES} are pre-sorted; only
   * US is hoisted). Each row renders an **SVG flag** — never an emoji, which Windows shows as bare
   * letters — via {@link flagSvg} + `{@html}`. A **Clear** control resets `value` to `''`.
   *
   * Self-contained and framework-pure beyond Svelte: no portal, just an absolutely-positioned
   * popover that closes on outside-click / `Esc`. Field-readable (large text, dark default) like the
   * rest of the console.
   */
  import { COUNTRIES, COUNTRY_NAME, US_CODE, type Country } from '../lib/countries.js';
  import { flagSvg } from '../lib/flags.js';

  let {
    value = $bindable(''),
    id = undefined,
    placeholder = 'Select a country…'
  }: {
    /** The stored alpha-2 code (uppercase), or `''` for none. Two-way bound. */
    value?: string;
    /** Optional id for the trigger button (so a `<label>` can point at it). */
    id?: string;
    placeholder?: string;
  } = $props();

  let open = $state(false);
  let query = $state('');
  let root = $state<HTMLDivElement>();
  let searchInput = $state<HTMLInputElement>();

  /** US first, then the rest alphabetically (COUNTRIES is already name-sorted). */
  const ordered = $derived.by(() => {
    const us = COUNTRIES.find((c) => c.code === US_CODE);
    const rest = COUNTRIES.filter((c) => c.code !== US_CODE);
    return us ? [us, ...rest] : rest;
  });

  /** The ordered list filtered by the search query (matches name or code, case-insensitive). */
  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (!q) return ordered;
    return ordered.filter(
      (c) => c.name.toLowerCase().includes(q) || c.code.toLowerCase().includes(q)
    );
  });

  /** The display name of the currently-selected code, if any. */
  const selectedName = $derived(value ? (COUNTRY_NAME[value.toUpperCase()] ?? value) : '');

  function toggle() {
    open = !open;
    if (open) {
      query = '';
      // Focus the search box once the popover is in the DOM.
      queueMicrotask(() => searchInput?.focus());
    }
  }

  function choose(c: Country) {
    value = c.code;
    open = false;
  }

  function clear(e: Event) {
    e.stopPropagation();
    value = '';
    open = false;
  }

  function onWindowClick(e: MouseEvent) {
    if (open && root && !root.contains(e.target as Node)) open = false;
  }

  function onWindowKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && open) open = false;
  }
</script>

<svelte:window onclick={onWindowClick} onkeydown={onWindowKeydown} />

<div class="country-select" bind:this={root}>
  <button
    type="button"
    {id}
    class="cs-trigger"
    class:is-empty={!value}
    aria-haspopup="listbox"
    aria-expanded={open}
    aria-label="Country"
    onclick={toggle}
  >
    {#if value}
      <!-- Flag markup is a static, self-contained <svg> string from `country-flag-icons`, keyed by a
           fixed alpha-2 code — no user input, no scripting. -->
      <!-- eslint-disable-next-line svelte/no-at-html-tags -->
      <span class="cs-flag" aria-hidden="true">{@html flagSvg(value) ?? ''}</span>
      <span class="cs-name">{selectedName}</span>
      <span class="cs-code">{value.toUpperCase()}</span>
      <!-- A clear control (an inline span, not a nested <button>, to stay valid inside the trigger). -->
      <span
        class="cs-clear"
        role="button"
        tabindex="0"
        aria-label="Clear country"
        onclick={clear}
        onkeydown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') clear(e);
        }}>×</span
      >
    {:else}
      <span class="cs-placeholder">{placeholder}</span>
    {/if}
    <span class="cs-chev" aria-hidden="true"></span>
  </button>

  {#if open}
    <div class="cs-popover">
      <input
        bind:this={searchInput}
        bind:value={query}
        class="cs-search"
        type="text"
        placeholder="Search countries…"
        aria-label="Search countries"
        autocomplete="off"
      />
      <ul class="cs-list" role="listbox" aria-label="Countries">
        {#each filtered as c (c.code)}
          <li>
            <button
              type="button"
              class="cs-option"
              class:selected={c.code === value}
              role="option"
              aria-selected={c.code === value}
              onclick={() => choose(c)}
            >
              <!-- Static SVG flag string from the dependency (see above). -->
              <!-- eslint-disable-next-line svelte/no-at-html-tags -->
              <span class="cs-flag" aria-hidden="true">{@html flagSvg(c.code) ?? ''}</span>
              <span class="cs-opt-name">{c.name}</span>
              <span class="cs-code">{c.code}</span>
            </button>
          </li>
        {:else}
          <li class="cs-empty">No matches</li>
        {/each}
      </ul>
    </div>
  {/if}
</div>

<style>
  .country-select {
    position: relative;
    width: 100%;
  }
  .cs-trigger {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    width: 100%;
    box-sizing: border-box;
    height: 2.25rem;
    padding: 0 var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    cursor: pointer;
    text-align: left;
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      box-shadow var(--gf-motion-fast) var(--gf-ease-out);
  }
  .cs-trigger:hover {
    border-color: var(--gf-border-strong);
  }
  .cs-trigger:focus-visible {
    outline: none;
    border-color: var(--gf-accent);
    box-shadow: 0 0 0 3px var(--gf-accent-soft);
  }
  .cs-placeholder {
    color: var(--gf-text-faint);
    flex: 1;
  }
  .cs-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cs-code {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-faint);
    font-variant-numeric: tabular-nums;
    letter-spacing: var(--gf-tracking-caps);
  }
  .cs-clear {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 1.25rem;
    height: 1.25rem;
    border-radius: var(--gf-radius-pill);
    color: var(--gf-text-muted);
    font-size: 1.1rem;
    line-height: 1;
    cursor: pointer;
  }
  .cs-clear:hover {
    background: var(--gf-surface-alt);
    color: var(--gf-danger);
  }
  .cs-chev {
    width: 0;
    height: 0;
    border-left: 4px solid transparent;
    border-right: 4px solid transparent;
    border-top: 5px solid var(--gf-text-muted);
    flex-shrink: 0;
  }
  .cs-flag {
    display: inline-flex;
    width: 1.5rem;
    flex-shrink: 0;
    border-radius: 2px;
    overflow: hidden;
    box-shadow: 0 0 0 1px var(--gf-border-subtle);
  }
  .cs-flag :global(svg) {
    display: block;
    width: 100%;
    height: auto;
  }

  .cs-popover {
    position: absolute;
    z-index: 50;
    top: calc(100% + 4px);
    left: 0;
    right: 0;
    display: flex;
    flex-direction: column;
    max-height: 18rem;
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-elevated);
    box-shadow: var(--gf-shadow-lg, 0 8px 24px rgba(0, 0, 0, 0.4));
    overflow: hidden;
  }
  .cs-search {
    margin: var(--gf-space-2);
    height: 2rem;
    padding: 0 var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
  }
  .cs-search:focus {
    outline: none;
    border-color: var(--gf-accent);
    box-shadow: 0 0 0 3px var(--gf-accent-soft);
  }
  .cs-list {
    list-style: none;
    margin: 0;
    padding: 0 var(--gf-space-1) var(--gf-space-1);
    overflow-y: auto;
  }
  .cs-option {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    width: 100%;
    padding: var(--gf-space-2) var(--gf-space-2);
    border: none;
    border-radius: var(--gf-radius-sm);
    background: transparent;
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    cursor: pointer;
    text-align: left;
  }
  .cs-option:hover {
    background: var(--gf-surface-alt);
  }
  .cs-option.selected {
    background: var(--gf-accent-soft);
    color: var(--gf-accent);
  }
  .cs-opt-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cs-empty {
    padding: var(--gf-space-3);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    text-align: center;
  }
</style>
