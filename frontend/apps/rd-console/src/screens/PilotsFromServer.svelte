<script lang="ts">
  /**
   * PilotsFromServer — bring pilots in from the linked server, already tied to their
   * accounts (docs/primitives.md §13.7).
   *
   * Two kinds of thing can come in:
   *
   * - a **pilot the club has on the server** and this Director does not. It comes in as it
   *   is there, so what it flies here goes to the same pilot on the server;
   * - a **member with no pilot**. A new pilot is made for them, named as they are.
   *
   * Nothing is added until the director picks and presses the button.
   */
  import { Banner, Button, Dialog } from '@gridfpv/components';
  import type { Pilot } from '@gridfpv/types';
  import { addWords, toBringIn, type ToBringIn } from '../lib/serverPilots.js';
  import type { Session } from '../lib/session.svelte.js';

  let {
    session,
    mine,
    onclose,
    onadded
  }: {
    session: Pick<Session, 'getServerPeople' | 'pullFromServer'>;
    /** The pilots this Director has now. */
    mine: readonly Pilot[];
    onclose: () => void;
    /** Pilots were added. */
    onadded: (added: Pilot[]) => void;
  } = $props();

  let offered = $state<ToBringIn | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  let pilots = $state<string[]>([]);
  let members = $state<string[]>([]);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  $effect(() => {
    let stopped = false;
    session
      .getServerPeople()
      .then((found) => {
        if (stopped) return;
        if (!found) return onclose();
        offered = toBringIn(found, mine);
      })
      .catch((err: unknown) => {
        if (!stopped) loadError = say(err);
      });
    return () => {
      stopped = true;
    };
  });

  const count = $derived(pilots.length + members.length);
  const nothing = $derived(
    offered !== undefined && offered.pilots.length === 0 && offered.members.length === 0
  );

  function pickAll() {
    pilots = offered?.pilots.map((pilot) => pilot.id) ?? [];
    members = offered?.members.map((member) => member.id) ?? [];
  }

  async function add() {
    if (busy || count === 0) return;
    busy = true;
    error = undefined;
    try {
      const added = await session.pullFromServer({ pilots, members });
      if (added) onadded(added);
    } catch (err) {
      error = say(err);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog open={true} title="Add pilots from the server" {onclose}>
  <div class="body" data-testid="pilots-from-server">
    {#if loadError}
      <Banner tone="danger" title="The server could not be asked.">{loadError}</Banner>
    {:else if !offered}
      <p class="muted" role="status">Asking the server…</p>
    {:else if nothing}
      <p>There is nobody to add. Every pilot and member on the server is already here.</p>
    {:else}
      {#if error}<Banner tone="danger">{error}</Banner>{/if}
      {#if offered.pilots.length > 0}
        <fieldset disabled={busy}>
          <legend>Pilots your club has on the server</legend>
          {#each offered.pilots as pilot (pilot.id)}
            <label>
              <input type="checkbox" bind:group={pilots} value={pilot.id} />
              <span class="name">{pilot.callsign}</span>
              {#if pilot.account}<span class="muted">{pilot.account.name}'s account</span>{/if}
            </label>
          {/each}
        </fieldset>
      {/if}
      {#if offered.members.length > 0}
        <fieldset disabled={busy}>
          <legend>Members with no pilot yet</legend>
          <p class="muted">A new pilot is made for each one you pick, with their name.</p>
          {#each offered.members as member (member.id)}
            <label>
              <input type="checkbox" bind:group={members} value={member.id} />
              <span class="name">{member.name}</span>
            </label>
          {/each}
        </fieldset>
      {/if}
      <div>
        <Button size="sm" variant="ghost" disabled={busy} onclick={pickAll}>Pick all</Button>
      </div>
    {/if}
  </div>
  {#snippet footer()}
    <Button variant="ghost" onclick={onclose} disabled={busy}>Cancel</Button>
    {#if offered && !nothing}
      <Button variant="primary" onclick={add} loading={busy} disabled={count === 0}
        >{addWords(count)}</Button
      >
    {/if}
  {/snippet}
</Dialog>

<style>
  .body {
    display: grid;
    gap: var(--gf-space-3);
  }
  p {
    margin: 0;
  }
  fieldset {
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    padding: var(--gf-space-3);
    margin: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  legend {
    font-weight: var(--gf-font-weight-semibold);
    padding: 0 var(--gf-space-1);
  }
  label {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  .name {
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
