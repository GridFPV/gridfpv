<script lang="ts">
  import type { HeatResult } from '@gridfpv/types';

  // Placeholder leaderboard. The real widget (positions, splits, deltas) is
  // built out in later issues; this exists so the library builds and to show the
  // shape: one component consuming the generated protocol types, themed per
  // surface (console dense, PWA mobile, overlay transparent).
  let { result }: { result: HeatResult } = $props();
</script>

<ol class="gridfpv-leaderboard">
  {#each result.places as place (place.competitor.competitor)}
    <li>
      <span class="pos">{place.position}</span>
      <span class="pilot">{place.competitor.competitor}</span>
    </li>
  {/each}
</ol>

<style>
  .gridfpv-leaderboard {
    margin: 0;
    padding: 0;
    list-style: none;
    font:
      14px/1.4 system-ui,
      sans-serif;
  }
  .gridfpv-leaderboard li {
    display: flex;
    gap: 0.5em;
  }
  .pos {
    width: 1.5em;
    text-align: right;
    opacity: 0.6;
  }
</style>
