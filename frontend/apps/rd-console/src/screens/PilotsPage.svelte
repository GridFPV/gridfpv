<script lang="ts">
  /**
   * PilotsPage — the Pilots page: the pilot list, which outlives events.
   *
   * It hosts the {@link PilotManager} (add, edit, remove). The Classes list sits beside it,
   * one tab over. Who races in which class at an event is set on the Event page.
   */
  import { Button, Card } from '@gridfpv/components';
  import type { Session } from '../lib/session.svelte.js';
  import Page from '../Page.svelte';
  import PageTabs from '../PageTabs.svelte';
  import { PEOPLE_TABS } from '../lib/tabs.js';
  import PilotManager from './PilotManager.svelte';

  let { session }: { session: Session } = $props();

  let manager = $state<PilotManager | undefined>(undefined);

  // Load the directory once on mount, so the page always reflects the current pilots.
  $effect(() => {
    void manager?.load();
  });
</script>

<Page
  title="Pilots"
  help="pilots"
  lede="Everyone who flies with you. Kept once here and used by every event. A pilot needs only a callsign."
>
  {#snippet actions()}
    <Button variant="primary" onclick={() => manager?.openAdd()}>+ Add pilot</Button>
  {/snippet}
  <PageTabs tabs={PEOPLE_TABS} current="pilots" label="Pilots and classes" />
  <Card elevation="sm">
    <div class="manager-wrap">
      <PilotManager bind:this={manager} {session} />
    </div>
  </Card>
</Page>

<style>
  .manager-wrap {
    padding: var(--gf-space-2);
  }
</style>
