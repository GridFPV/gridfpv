<script lang="ts">
  /**
   * Test harness for Collapsible — exercises the `summary`/`actions`/`children` snippets and a
   * bound `open` prop (so a unit test can assert the collapse propagates back to the parent).
   */
  import Collapsible from '../src/primitives/Collapsible.svelte';

  let { title = 'Class A', open = $bindable(true) }: { title?: string; open?: boolean } = $props();
</script>

<Collapsible {title} bind:open>
  {#snippet summary()}
    <span>3 placed</span>
  {/snippet}
  {#snippet actions()}
    <button type="button" data-testid="action-btn">Save</button>
  {/snippet}
  <div data-testid="body">Body content</div>
</Collapsible>

<span data-testid="open-probe">{open}</span>
