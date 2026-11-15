<script lang="ts">
  /**
   * DiscardDialog — throw a stopped race away, with an optional reason.
   *
   * The reason is one tap: each reason button discards the race straight away. "False
   * start" asks one more thing first, which pilot it was, so the false start is recorded
   * against them. The parent mounts this only while it is open.
   */
  import { Banner, Button, Dialog } from '@gridfpv/components';
  import type { DiscardReason, PilotId } from '@gridfpv/types';
  import { DISCARD_REASONS } from '../lib/run.js';

  let {
    pilots,
    ondiscard,
    onclose
  }: {
    /** The pilots in the race, with their callsigns, in seat order. */
    pilots: readonly { id: PilotId; callsign: string }[];
    /**
     * Discard the race. Resolves once it is done; a rejection's message is shown here.
     * `falseStartBy` is set when the director named who jumped the start.
     */
    ondiscard: (reason: DiscardReason | undefined, falseStartBy?: PilotId) => Promise<void>;
    onclose: () => void;
  } = $props();

  /** The second step: asking who made the false start. */
  let askingWho = $state(false);
  let busy = $state(false);
  let error = $state<string | undefined>(undefined);

  async function discard(reason: DiscardReason | undefined, falseStartBy?: PilotId) {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      await ondiscard(reason, falseStartBy);
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  function pick(reason: DiscardReason) {
    // With no named pilot in the race there is nobody to ask about.
    if (reason === 'FalseStart' && pilots.length > 0) askingWho = true;
    else void discard(reason);
  }
</script>

<Dialog
  open={true}
  title={askingWho ? 'Who made the false start?' : 'Discard this race?'}
  {onclose}
>
  <div class="body" aria-label="Discard race">
    {#if askingWho}
      <p>The false start is recorded against the pilot you pick. The race is discarded.</p>
      <div class="choices" role="group" aria-label="Pilot who made the false start">
        {#each pilots as pilot (pilot.id)}
          <Button block disabled={busy} onclick={() => discard('FalseStart', pilot.id)}>
            {pilot.callsign}
          </Button>
        {/each}
      </div>
    {:else}
      <p>Its laps will not count. Pick a reason, or discard without one.</p>
      <div class="choices" role="group" aria-label="Reason">
        {#each DISCARD_REASONS as entry (entry.reason)}
          <Button block disabled={busy} onclick={() => pick(entry.reason)}>{entry.label}</Button>
        {/each}
      </div>
    {/if}
    {#if error}
      <Banner tone="danger">{error}</Banner>
    {/if}
  </div>
  {#snippet footer()}
    {#if askingWho}
      <Button variant="ghost" disabled={busy} onclick={() => (askingWho = false)}>Back</Button>
    {:else}
      <Button variant="ghost" disabled={busy} onclick={onclose}>Keep the race</Button>
      <Button variant="danger" disabled={busy} onclick={() => discard(undefined)}>
        Discard, no reason
      </Button>
    {/if}
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
  .choices {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--gf-space-2);
  }
</style>
