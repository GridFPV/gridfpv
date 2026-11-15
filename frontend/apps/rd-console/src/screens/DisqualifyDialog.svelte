<script lang="ts">
  /**
   * DisqualifyDialog — disqualify one pilot from one phase. The director is asked two things:
   * an optional reason, and what happens to a place the pilot had already earned in a later
   * heat. By default the next pilot takes it; the other choice keeps it empty.
   *
   * Nothing here is automatic. Grid may propose a disqualification, and this dialog is where
   * the director decides. The parent mounts it only while it is open.
   */
  import { Banner, Button, Dialog, Field, Input } from '@gridfpv/components';

  let {
    callsign,
    phaseLabel,
    ondisqualify,
    onclose
  }: {
    callsign: string;
    phaseLabel: string;
    /** Disqualify. Resolves once done; a rejection's message is shown here. */
    ondisqualify: (reason: string | undefined, keepPlaceEmpty: boolean) => Promise<void>;
    onclose: () => void;
  } = $props();

  let reason = $state('');
  let place = $state<'next' | 'empty'>('next');
  let busy = $state(false);
  let error = $state<string | undefined>(undefined);

  async function submit(e?: Event) {
    e?.preventDefault();
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      await ondisqualify(reason.trim() || undefined, place === 'empty');
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog open={true} title="Disqualify {callsign}?" {onclose}>
  <form class="body" aria-label="Disqualify {callsign}" onsubmit={submit}>
    <p>
      {callsign} is dropped from later heats in {phaseLabel} and ranked below everyone who kept racing.
      This can be undone with Reinstate.
    </p>
    <Field label="Reason (optional)">
      <Input
        bind:value={reason}
        placeholder="e.g. Two false starts"
        aria-label="Reason"
        autocomplete="off"
      />
    </Field>
    <fieldset class="choice">
      <legend>A place {callsign} had already earned in a later heat</legend>
      <label>
        <input type="radio" name="place" value="next" bind:group={place} disabled={busy} />
        The next pilot takes their place
      </label>
      <label>
        <input type="radio" name="place" value="empty" bind:group={place} disabled={busy} />
        Keep their place empty
      </label>
    </fieldset>
    {#if error}
      <Banner tone="danger">{error}</Banner>
    {/if}
  </form>
  {#snippet footer()}
    <Button variant="ghost" disabled={busy} onclick={onclose}>Cancel</Button>
    <Button variant="danger" disabled={busy} onclick={() => submit()}>Disqualify</Button>
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
  .choice {
    margin: 0;
    padding: 0;
    border: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  .choice legend {
    padding: 0;
    margin-bottom: var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
  }
  .choice label {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
</style>
