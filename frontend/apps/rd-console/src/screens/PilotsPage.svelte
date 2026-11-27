<script lang="ts">
  /**
   * PilotsPage — the Pilots page: the pilot list, which outlives events.
   *
   * It hosts the {@link PilotManager} (add, edit, remove). Who races in which class at an
   * event is set on that event's Details page.
   *
   * With a server linked (docs/primitives.md §13.7) it also offers two things that ask the
   * server: saying which account a pilot is there, and bringing pilots in from it already
   * tied to their accounts.
   */
  import { Button, Card, toast } from '@gridfpv/components';
  import type { Pilot } from '@gridfpv/types';
  import { serverLinked } from '../lib/serverPilots.js';
  import type { Session } from '../lib/session.svelte.js';
  import Page from '../Page.svelte';
  import PilotAccountDialog from './PilotAccountDialog.svelte';
  import PilotManager from './PilotManager.svelte';
  import PilotsFromServer from './PilotsFromServer.svelte';

  let { session }: { session: Session } = $props();

  let manager = $state<PilotManager | undefined>(undefined);
  let pilots = $state<Pilot[]>([]);
  /** Whether a server is linked and switched on. */
  let linked = $state(false);
  /** The pilot whose account is being set. */
  let tying = $state<Pilot | undefined>(undefined);
  let bringing = $state(false);

  // Load the directory once on mount, so the page always reflects the current pilots.
  $effect(() => {
    void manager?.load();
  });

  $effect(() => {
    session
      .getServerLink()
      .then((view) => (linked = serverLinked(view)))
      // Not knowing is the same as no server: the page works as it always has.
      .catch(() => (linked = false));
  });

  function saved(pilot: Pilot) {
    tying = undefined;
    toast.success(
      pilot.server_account
        ? `${pilot.callsign} is ${pilot.server_account.name} on the server.`
        : `${pilot.callsign} is nobody on the server now.`
    );
    void manager?.load();
  }

  function added(list: Pilot[]) {
    bringing = false;
    toast.success(
      list.length === 1 ? `Added “${list[0].callsign}”.` : `Added ${list.length} pilots.`
    );
    void manager?.load();
  }
</script>

<Page
  title="Pilots"
  help="pilots"
  lede="Everyone who flies with you. Kept once here and used by every event. A pilot needs only a callsign."
>
  {#snippet actions()}
    {#if linked}
      <Button onclick={() => (bringing = true)}>Add from the server</Button>
    {/if}
    <Button variant="primary" onclick={() => manager?.openAdd()}>+ Add pilot</Button>
  {/snippet}
  <Card elevation="sm">
    <div class="manager-wrap">
      <PilotManager bind:this={manager} bind:pilots {session}>
        {#snippet rowActions(pilot: Pilot)}
          {#if linked}
            <Button
              variant="ghost"
              size="sm"
              aria-label="Account of {pilot.callsign} on the server"
              onclick={() => (tying = pilot)}>Account</Button
            >
          {/if}
        {/snippet}
      </PilotManager>
    </div>
  </Card>
</Page>

{#if tying}
  <PilotAccountDialog {session} pilot={tying} onclose={() => (tying = undefined)} onsaved={saved} />
{/if}
{#if bringing}
  <PilotsFromServer {session} mine={pilots} onclose={() => (bringing = false)} onadded={added} />
{/if}

<style>
  .manager-wrap {
    padding: var(--gf-space-2);
  }
</style>
