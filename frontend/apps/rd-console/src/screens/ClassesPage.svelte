<script lang="ts">
  /**
   * ClassesPage — the Classes page: the class list, which outlives events.
   *
   * It hosts the {@link ClassManager}: the standard FPV classes are built in and locked, and
   * a director adds their own. This list is only the names. Which classes an event races,
   * and who is in each, is set on that event's Details page, and the page says so.
   */
  import { Button, Card } from '@gridfpv/components';
  import type { Session } from '../lib/session.svelte.js';
  import Page from '../Page.svelte';
  import { formatHash } from '../lib/route.js';
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
  <p class="where">
    This list is only the names. Who races in a class is set for each event, on its
    <a href={formatHash({ kind: 'event', section: 'event' })}>Details page</a>.
  </p>
  <Card elevation="sm">
    <div class="manager-wrap">
      <ClassManager bind:this={manager} {session} manageHidden />
    </div>
  </Card>
</Page>

<style>
  .where {
    margin: 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .where a {
    color: var(--gf-accent);
  }
  .manager-wrap {
    padding: var(--gf-space-2);
  }
</style>
