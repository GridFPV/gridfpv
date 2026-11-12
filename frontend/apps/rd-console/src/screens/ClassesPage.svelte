<script lang="ts">
  /**
   * ClassesPage — the Classes page: the class list, which outlives events.
   *
   * It hosts the {@link ClassManager}: the standard FPV classes are built in and locked, and
   * a director adds their own. It sits beside the Pilots page, one tab over. Which classes
   * an event races, and who is in each, is set on the Event page.
   */
  import { Button, Card } from '@gridfpv/components';
  import type { Session } from '../lib/session.svelte.js';
  import Page from '../Page.svelte';
  import PageTabs from '../PageTabs.svelte';
  import { PEOPLE_TABS } from '../lib/tabs.js';
  import ClassManager from './ClassManager.svelte';

  let { session }: { session: Session } = $props();

  let manager = $state<ClassManager | undefined>(undefined);

  // Load the directory once on mount, so the page always reflects the current classes.
  $effect(() => {
    void manager?.load();
  });
</script>

<Page
  title="Classes"
  help="pilots"
  lede="The classes an event can race. The standard FPV classes are built in; add your own, which need only a name."
>
  {#snippet actions()}
    <Button variant="primary" onclick={() => manager?.openAdd()}>+ Add class</Button>
  {/snippet}
  <PageTabs tabs={PEOPLE_TABS} current="classes" label="Pilots and classes" />
  <Card elevation="sm">
    <div class="manager-wrap">
      <ClassManager bind:this={manager} {session} manageHidden />
    </div>
  </Card>
</Page>

<style>
  .manager-wrap {
    padding: var(--gf-space-2);
  }
</style>
