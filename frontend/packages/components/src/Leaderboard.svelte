<script lang="ts">
  import type { RaceSnapshot } from '@gridfpv/types';

  // Placeholder leaderboard. The real widget (positions, splits, deltas) is
  // built out in later issues; this exists so the library builds and to show the
  // shape: one component consuming the generated protocol types, themed per
  // surface (console dense, PWA mobile, overlay transparent).
  let { snapshot }: { snapshot: RaceSnapshot } = $props();
</script>

<ol class="gridfpv-leaderboard">
  {#each snapshot.pilots as pilotId, i (pilotId)}
    <li><span class="pos">{i + 1}</span><span class="pilot">{pilotId}</span></li>
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
