<script lang="ts">
  /**
   * TimersPage — the app-level **Timers** page (#118).
   *
   * Replaces the old picker-header "Timers" **modal** (`Timers.svelte`): app-level timer
   * management now lives on a dedicated page in the home hub's two-level IA. It still **reuses the
   * shared {@link TimerManager}** (CRUD-only, no per-event selection) — the component is unchanged;
   * only its host moved from a `Dialog` to a routed page. Reached via Home › Timers; `Breadcrumbs`
   * + the brand root get you home from here.
   *
   * (The in-event Timers screen — {@link EventTimers}, which layers per-event selection on top of
   * the same manager — is untouched; this is the no-event, registry-wide surface.)
   */
  import { Button, Card, InfoTip } from '@gridfpv/components';
  import type { Session } from '../lib/session.svelte.js';
  import Brand from '../Brand.svelte';
  import Breadcrumbs from '../Breadcrumbs.svelte';
  import TimerManager from './TimerManager.svelte';

  let {
    session,
    onhome,
    ontune
  }: {
    session: Session;
    onhome: () => void;
    /** Open the per-timer Tune page (#355) — the shell owns the route; the row is the entry point. */
    ontune?: (timerId: string) => void;
  } = $props();

  let manager = $state<TimerManager | undefined>(undefined);

  // Load the registry once on mount, so the page always reflects the current timers.
  $effect(() => {
    void manager?.load();
  });
</script>

<div class="page">
  <div class="page-inner">
    <!-- The brand is the home root here too (#118): the directory pages carry the same
         top-left GridFPV mark as the workspace sidebar, leaving for the hub. -->
    <div class="brand-row"><Brand onclick={onhome} /></div>
    <Breadcrumbs crumbs={[{ label: 'Home', onclick: onhome }, { label: 'Timers' }]} />

    <header class="page-head">
      <div class="page-titles">
        <div class="page-titleline">
          <h1 class="page-title">Timers</h1>
          <!-- #466: moved off the page — reference material about how timers are scoped. -->
          <InfoTip
            label="About timers"
            text="Timers are configured once and reused across events. Each event picks which timers it uses; the built-in Mock source flies a synthetic race with no hardware."
          />
        </div>
      </div>
      <Button variant="primary" onclick={() => manager?.openAdd()}>+ Add timer</Button>
    </header>

    <Card elevation="sm">
      <div class="manager-wrap">
        <TimerManager
          bind:this={manager}
          {session}
          ontune={ontune ? (timer) => ontune(timer.id) : undefined}
        />
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
