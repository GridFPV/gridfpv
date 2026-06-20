<script lang="ts">
  // Placeholder race clock. Renders elapsed milliseconds as M:SS.mmm.
  // A real implementation will tick off the protocol's authoritative time.
  let { elapsedMs = 0 }: { elapsedMs?: number } = $props();

  let label = $derived.by(() => {
    const totalMs = Math.max(0, Math.floor(elapsedMs));
    const minutes = Math.floor(totalMs / 60000);
    const seconds = Math.floor((totalMs % 60000) / 1000);
    const millis = totalMs % 1000;
    return `${minutes}:${String(seconds).padStart(2, '0')}.${String(millis).padStart(3, '0')}`;
  });
</script>

<span class="gridfpv-race-clock">{label}</span>

<style>
  .gridfpv-race-clock {
    font:
      600 16px/1 ui-monospace,
      monospace;
    font-variant-numeric: tabular-nums;
  }
</style>
