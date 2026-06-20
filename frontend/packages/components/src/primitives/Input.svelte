<script lang="ts">
  /**
   * Input — a token-styled text/number/url/password input with two-way `value`
   * binding. Forwards arbitrary attributes (type, placeholder, aria-label,
   * step, autocomplete, …) so it drops in anywhere a raw `<input>` was used.
   */
  let {
    value = $bindable(''),
    invalid = false,
    size = 'md',
    // Normal component (not a custom element); attribute forwarding is intentional.
    // eslint-disable-next-line svelte/valid-compile
    ...rest
  }: {
    value?: string | number;
    invalid?: boolean;
    size?: 'sm' | 'md';
    [key: string]: unknown;
  } = $props();
</script>

<input class="gf-input" data-size={size} aria-invalid={invalid || undefined} bind:value {...rest} />

<style>
  .gf-input {
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
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      box-shadow var(--gf-motion-fast) var(--gf-ease-out);
  }
  .gf-input[data-size='sm'] {
    height: 1.85rem;
    font-size: var(--gf-font-size-xs);
  }
  .gf-input::placeholder {
    color: var(--gf-text-faint);
  }
  .gf-input:hover:not(:disabled) {
    border-color: var(--gf-border-strong);
  }
  .gf-input:focus {
    outline: none;
    border-color: var(--gf-accent);
    box-shadow: 0 0 0 3px var(--gf-accent-soft);
  }
  .gf-input[aria-invalid='true'] {
    border-color: var(--gf-danger);
  }
  .gf-input[aria-invalid='true']:focus {
    box-shadow: 0 0 0 3px var(--gf-danger-soft);
  }
  .gf-input:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }
</style>
