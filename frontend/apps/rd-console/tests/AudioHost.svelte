<script lang="ts">
  /**
   * Test host for the APP-WIDE race-day audio controller: the real console mounts it once in
   * `App.svelte`; audio-behaviour tests render this next to the screen under test to get the
   * same wiring (the controller consumes the same test session's live stream).
   */
  import { mountRaceDayAudio } from '../src/lib/raceDayAudio.svelte.js';
  import type { Session } from '../src/lib/session.svelte.js';

  let { session }: { session: Session } = $props();
  // The controller binds to ONE session for its lifetime (exactly like App.svelte's mount);
  // tests never swap the prop, so capturing the initial value is the intent.
  // eslint-disable-next-line svelte/valid-compile
  // svelte-ignore state_referenced_locally
  mountRaceDayAudio(session);
</script>
