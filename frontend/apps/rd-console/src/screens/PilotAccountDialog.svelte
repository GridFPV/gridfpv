<script lang="ts">
  /**
   * PilotAccountDialog — say which account a pilot is on the linked server
   * (docs/primitives.md §13.7).
   *
   * The director picks one of the club's members by name. Nobody types an id. The pilot's
   * results on the server then show on that member's account. The server is asked when the
   * dialog opens and when it is saved, so it must be in reach.
   */
  import { Banner, Button, Dialog } from '@gridfpv/components';
  import type { LinkPeople, Pilot } from '@gridfpv/types';
  import { heldElsewhere } from '../lib/serverPilots.js';
  import type { Session } from '../lib/session.svelte.js';

  let {
    session,
    pilot,
    onclose,
    onsaved
  }: {
    session: Pick<Session, 'getServerPeople' | 'setPilotServerAccount'>;
    pilot: Pilot;
    onclose: () => void;
    /** The server agreed. Here is the pilot as it now stands. */
    onsaved: (pilot: Pilot) => void;
  } = $props();

  let people = $state<LinkPeople | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  /** The member picked. Empty for nobody. */
  let picked = $state('');

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  $effect(() => {
    const id = pilot.id;
    let stopped = false;
    people = undefined;
    loadError = undefined;
    session
      .getServerPeople()
      .then((found) => {
        if (stopped) return;
        if (!found) return onclose();
        people = found;
        // Start from what the server says now, which may be newer than what is kept here.
        picked = found.pilots.find((p) => p.id === id)?.account?.id ?? '';
      })
      .catch((err: unknown) => {
        if (!stopped) loadError = say(err);
      });
    return () => {
      stopped = true;
    };
  });

  const current = $derived(people?.pilots.find((p) => p.id === pilot.id)?.account?.id ?? '');
  const elsewhere = $derived(people ? heldElsewhere(people, pilot.id) : new Map<string, string>());

  async function save() {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      const saved = await session.setPilotServerAccount(
        pilot.id,
        picked === '' ? undefined : picked
      );
      if (saved) onsaved(saved);
    } catch (err) {
      error = say(err);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog open={true} title="Who is {pilot.callsign} on the server?" {onclose}>
  <div class="body" data-testid="pilot-account">
    {#if loadError}
      <Banner tone="danger" title="The server could not be asked.">{loadError}</Banner>
    {:else if !people}
      <p class="muted" role="status">Asking the server…</p>
    {:else if people.members.length === 0}
      <p>
        Your club has no members on the server yet. Invite them from the club's page on the server.
      </p>
    {:else}
      {#if error}<Banner tone="danger">{error}</Banner>{/if}
      <label for="pilot-account-pick">Account</label>
      <select id="pilot-account-pick" bind:value={picked} disabled={busy}>
        <option value="">Nobody</option>
        {#each people.members as member (member.id)}
          <option value={member.id}
            >{member.name}{elsewhere.has(member.id)
              ? ` (also ${elsewhere.get(member.id)})`
              : ''}</option
          >
        {/each}
      </select>
      <p class="muted">
        {pilot.callsign}'s results on the server show on this person's account. The pilot takes the
        callsign, colour and ids that person has filled in on the server.
      </p>
    {/if}
  </div>
  {#snippet footer()}
    <Button variant="ghost" onclick={onclose} disabled={busy}>Cancel</Button>
    {#if people && people.members.length > 0}
      <Button variant="primary" onclick={save} loading={busy} disabled={picked === current}
        >Save</Button
      >
    {/if}
  {/snippet}
</Dialog>

<style>
  .body {
    display: grid;
    gap: var(--gf-space-2);
  }
  p {
    margin: 0;
  }
  label {
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
  }
  select {
    font: inherit;
    color: var(--gf-text);
    background: var(--gf-surface-sunken);
    border: 1px solid var(--gf-border-strong);
    border-radius: var(--gf-radius-sm);
    padding: var(--gf-space-2);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
