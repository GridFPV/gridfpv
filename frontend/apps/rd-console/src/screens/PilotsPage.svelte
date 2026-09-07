<script lang="ts">
  /**
   * PilotsPage — the app-level **Pilots** page (#118 / #74).
   *
   * A real route in the two-level IA's home hub (Home › Pilots). It hosts the shared
   * {@link PilotManager} (directory CRUD: the add / edit / remove registration form), mirroring how
   * {@link TimersPage} hosts {@link TimerManager} — the manager is reusable; only its host differs
   * (a routed page here; the in-event Roster will embed the same component with per-event selection
   * layered on next). `Breadcrumbs` + the brand root get you home from here.
   */
  import { Button, Card, InfoTip } from '@gridfpv/components';
  import type { Session } from '../lib/session.svelte.js';
  import Brand from '../Brand.svelte';
  import Breadcrumbs from '../Breadcrumbs.svelte';
  import PilotManager from './PilotManager.svelte';

  let { session, onhome }: { session: Session; onhome: () => void } = $props();

  let manager = $state<PilotManager | undefined>(undefined);

  // Load the directory once on mount, so the page always reflects the current pilots.
  $effect(() => {
    void manager?.load();
  });
</script>

<div class="page">
  <div class="page-inner">
    <!-- The brand is the home root here too (#118): the directory pages carry the same
         top-left GridFPV mark as the workspace sidebar, leaving for the hub. -->
    <div class="brand-row"><Brand onclick={onhome} /></div>
    <Breadcrumbs crumbs={[{ label: 'Home', onclick: onhome }, { label: 'Pilots' }]} />

    <header class="page-head">
      <div class="page-titles">
        <div class="page-titleline">
          <h1 class="page-title">Pilots</h1>
          <!-- #466: moved off the page. "Callsign is the only required field" is already carried by
               the required marker on the field itself. -->
          <InfoTip
            label="About the pilot directory"
            text="The application-level pilot directory — maintained once here, rostered per event. Each pilot needs only a callsign; the rest (team, country, colour, VTX, IDs) is optional."
          />
        </div>
      </div>
      <Button variant="primary" onclick={() => manager?.openAdd()}>+ Add pilot</Button>
    </header>

    <Card elevation="sm">
      <div class="manager-wrap">
        <PilotManager bind:this={manager} {session} />
      </div>
    </Card>
  </div>
</div>

<style>
  .brand-row {
    margin-bottom: var(--gf-space-4);
  }

  .page {
    min-height: 100vh;
    padding: var(--gf-space-6) var(--gf-space-8) var(--gf-space-8);
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    overflow: auto;
  }
  .page-inner {
    width: min(52rem, 100%);
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-5);
  }
  .page-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--gf-space-4);
  }
  .page-titles {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
    min-width: 0;
  }
  .page-titleline {
    display: flex;
    align-items: center;
  }
  .page-title {
    margin: 0;
    font-size: var(--gf-font-size-2xl);
    letter-spacing: var(--gf-tracking-tight);
  }
  .manager-wrap {
    padding: var(--gf-space-2);
  }
</style>
