<script lang="ts">
  /**
   * OverlaysPage — the pages that go on top of video in OBS, each with its URL, a choice of
   * contrast and a preview. The list itself is {@link EventOverlays}.
   */
  import { Card } from '@gridfpv/components';
  import Page from '../Page.svelte';
  import type { Session } from '../lib/session.svelte.js';
  import EventOverlays from './EventOverlays.svelte';

  let { session }: { session: Session } = $props();
</script>

<Page
  title="Overlays"
  help="overlays"
  lede="Pages that go on top of your video in OBS. Set them up once: they follow whichever event is open, and only read."
>
  <Card elevation="sm">
    <EventOverlays {session} />
  </Card>
</Page>
