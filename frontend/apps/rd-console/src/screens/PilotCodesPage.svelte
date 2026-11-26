<script lang="ts">
  /**
   * PilotCodesPage — the codes the event's pilots sign in with (docs/primitives.md §11.3).
   *
   * The Details page is what is set once for an event. The codes are used during it, as
   * pilots walk up, so they have a page of their own among the event's pages: show a pilot
   * their code, give one a new code, sign a phone out, and print a sheet of them all.
   * The work itself is {@link PilotSignIn}.
   */
  import type { EventMeta } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import type { Session } from '../lib/session.svelte.js';
  import PilotSignIn from './PilotSignIn.svelte';

  let { session, event, live }: { session: Session; event: EventMeta; live: EventLive } = $props();
</script>

<Page title="Pilot codes" help="event" lede="The codes this event's pilots sign in with.">
  <PilotSignIn {session} event={event.id} names={live.names} cursor={live.view?.cursor} />
</Page>
