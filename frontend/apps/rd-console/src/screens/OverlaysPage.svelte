<script lang="ts">
  /**
   * OverlaysPage — the pages that go on top of video in OBS, each with its URL, a choice of
   * contrast and a preview. The list itself is {@link EventOverlays}.
   */
  import { Card } from '@gridfpv/components';
  import Page from '../Page.svelte';
  import EventOverlays from './EventOverlays.svelte';
</script>

<Page
  title="Overlays"
  help="overlays"
  lede="Pages that go on top of your video in OBS. They follow the open event and only read."
>
  <Card elevation="sm">
    <EventOverlays />
  </Card>
</Page>
