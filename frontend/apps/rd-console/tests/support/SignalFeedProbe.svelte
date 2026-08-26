<script lang="ts">
  import { useSignalFeed } from '../../src/lib/signalFeed.svelte.js';
  let { id, read, release, pollMs } = $props();
  const feed = useSignalFeed({
    timer: () => id,
    read: () => read,
    release: () => release,
    pollMs: () => pollMs
  });
</script>

<span data-testid="streaming">{feed.streaming}</span>
