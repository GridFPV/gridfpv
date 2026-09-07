<script lang="ts">
  /**
   * ClassesPage — the app-level **Classes** page (#118 / #84).
   *
   * A real route in the two-level IA's home hub (Home › Classes). It hosts the shared
   * {@link ClassManager} (directory CRUD: the add / edit / remove form for Custom classes, plus the
   * locked, read-only built-ins the Director seeds), mirroring how {@link PilotsPage} hosts
   * {@link PilotManager} — the manager is reusable; only its
   * host differs (a routed page here; the in-event {@link EventClassesRoster} embeds the same
   * component with per-event selection layered on). `Breadcrumbs` + the brand root get you home.
   */
  import { Button, Card, InfoTip } from '@gridfpv/components';
  import type { Session } from '../lib/session.svelte.js';
  import Brand from '../Brand.svelte';
  import Breadcrumbs from '../Breadcrumbs.svelte';
  import ClassManager from './ClassManager.svelte';

  let { session, onhome }: { session: Session; onhome: () => void } = $props();

  let manager = $state<ClassManager | undefined>(undefined);

  // Load the directory once on mount, so the page always reflects the current classes.
  $effect(() => {
    void manager?.load();
  });
</script>

<div class="page">
  <div class="page-inner">
    <!-- The brand is the home root here too (#118): the directory pages carry the same
         top-left GridFPV mark as the workspace sidebar, leaving for the hub. -->
    <div class="brand-row"><Brand onclick={onhome} /></div>
    <Breadcrumbs crumbs={[{ label: 'Home', onclick: onhome }, { label: 'Classes' }]} />

    <header class="page-head">
      <div class="page-titles">
        <div class="page-titleline">
          <h1 class="page-title">Classes</h1>
          <!-- #466: the standing explainer moved into a tooltip. The built-in/Custom distinction is
               already visible on every row (a lock chip + a Badge), so this is reference, not news. -->
          <InfoTip
            label="About the class directory"
            text="The application-level class directory — maintained once here, selected per event. The standard FPV classes are built in (locked, identical on every Director); add your own Custom classes, which need only a name."
          />
        </div>
      </div>
      <Button variant="primary" onclick={() => manager?.openAdd()}>+ Add class</Button>
    </header>

    <Card elevation="sm">
      <div class="manager-wrap">
        <ClassManager bind:this={manager} {session} manageHidden />
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
