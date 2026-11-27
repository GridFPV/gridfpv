<script lang="ts">
  /**
   * Switch — an on/off switch, as the Network page has for its doors. It only says what was
   * asked for: the page decides whether to go ahead, and shows the new state when it has.
   */
  let {
    checked,
    label,
    disabled = false,
    onchange
  }: {
    /** Whether it is on now. */
    checked: boolean;
    /** What it switches, for a screen reader. */
    label: string;
    disabled?: boolean;
    /** Asked to switch to `next`. The switch stays as it is until `checked` changes. */
    onchange: (next: boolean) => void;
  } = $props();

  function flip(e: Event) {
    const box = e.currentTarget as HTMLInputElement;
    const next = box.checked;
    // The page says when it is so: until then the switch shows what is.
    box.checked = checked;
    onchange(next);
  }
</script>

<label class="switch">
  <input type="checkbox" role="switch" aria-label={label} {checked} {disabled} onchange={flip} />
  <span class="track" aria-hidden="true"><span class="thumb"></span></span>
</label>

<style>
  .switch {
    position: relative;
    display: inline-flex;
    cursor: pointer;
    flex-shrink: 0;
  }
  .switch input {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    margin: 0;
    opacity: 0;
    cursor: pointer;
    /* Above the drawn track and its thumb, so a click always lands on the box itself. */
    z-index: 1;
  }
  .switch input:disabled {
    cursor: default;
  }
  .track {
    width: 2.75rem;
    height: 1.5rem;
    border-radius: var(--gf-radius-pill);
    background: var(--gf-surface-sunken);
    border: 1px solid var(--gf-border-strong);
    transition: background var(--gf-motion-fast) var(--gf-ease-out);
    display: flex;
    align-items: center;
    padding: 0 2px;
    pointer-events: none;
  }
  .thumb {
    width: 1.125rem;
    height: 1.125rem;
    border-radius: 50%;
    background: var(--gf-text-muted);
    transition:
      transform var(--gf-motion-fast) var(--gf-ease-out),
      background var(--gf-motion-fast) var(--gf-ease-out);
  }
  .switch input:checked + .track {
    background: var(--gf-accent);
    border-color: var(--gf-accent);
  }
  .switch input:checked + .track .thumb {
    transform: translateX(1.2rem);
    background: var(--gf-accent-contrast);
  }
  .switch input:focus-visible + .track {
    outline: var(--gf-ring-width) solid var(--gf-ring-color);
    outline-offset: var(--gf-ring-offset);
  }
  .switch input:disabled + .track {
    opacity: 0.6;
  }
</style>
