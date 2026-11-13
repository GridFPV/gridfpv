<script lang="ts">
  /**
   * PilotsPage — the Pilots page: the pilot list, which outlives events.
   *
   * It hosts the {@link PilotManager} (add, edit, remove). Who races in which class at an
   * event is set on that event's Details page.
   */
  import { Button, Card } from '@gridfpv/components';
  import type { Session } from '../lib/session.svelte.js';
  import Page from '../Page.svelte';
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
