<script lang="ts">
  /**
   * SetPositionDialog — set one pilot's position in a heat by hand, with an optional note.
   * The position holds until the director clears it, and every lineup and ranking that reads
   * the heat sees it. The parent mounts this only while it is open.
   */
  import { Banner, Button, Dialog, Field, Input, Select } from '@gridfpv/components';
  import { ordinal } from '../lib/names.js';

  let {
    callsign,
    heatLabel,
    positions,
    current,
    onset,
    onclose
  }: {
    callsign: string;
    heatLabel: string;
    /** The positions on offer, 1 first. */
    positions: readonly number[];
    /** The pilot's position now. */
    current: number;
    /** Set the position. Resolves once done; a rejection's message is shown here. */
    onset: (position: number, note: string | undefined) => Promise<void>;
    onclose: () => void;
  } = $props();

  // The select holds a string; it starts on the pilot's position now.
  // svelte-ignore state_referenced_locally
  let position = $state(String(current));
  let note = $state('');
  let busy = $state(false);
  let error = $state<string | undefined>(undefined);

  async function submit(e?: Event) {
    e?.preventDefault();
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      await onset(Number(position), note.trim() || undefined);
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog open={true} title="Set {callsign}'s position" {onclose}>
  <form class="body" aria-label="Set {callsign}'s position" onsubmit={submit}>
    <p>
      This sets where {callsign} finished in {heatLabel}. It holds until you clear it, and later
      heats that take pilots from this one follow it.
    </p>
    <Field label="Position">
      <Select bind:value={position} aria-label="Position" disabled={busy}>
        {#each positions as choice (choice)}
          <option value={String(choice)}>{ordinal(choice)}</option>
        {/each}
      </Select>
    </Field>
    <Field label="Note (optional)">
      <Input
        bind:value={note}
        placeholder="e.g. Video review"
        aria-label="Note"
        autocomplete="off"
      />
    </Field>
    {#if error}
      <Banner tone="danger">{error}</Banner>
    {/if}
  </form>
  {#snippet footer()}
    <Button variant="ghost" disabled={busy} onclick={onclose}>Cancel</Button>
    <Button variant="primary" disabled={busy} onclick={() => submit()}>Set position</Button>
  {/snippet}
</Dialog>

<style>
  .body {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .body p {
    margin: 0;
    color: var(--gf-text-secondary);
  }
</style>
