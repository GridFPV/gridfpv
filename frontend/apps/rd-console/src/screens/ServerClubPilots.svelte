<script lang="ts">
  /**
   * ServerClubPilots — a club's pilots and the accounts that hold them, for those who
   * look after the club (docs/primitives.md §13.7).
   *
   * A pilot here is one a Director of the club sent. The account that holds a pilot gets
   * the results flown under it. A director gives a pilot to a member, or takes it back. A
   * claims approver only answers the members who asked, and not their own ask.
   */
  import { Banner, Button, Card, Dialog, InfoTip } from '@gridfpv/components';
  import type { ClubPilotView, MemberView, PushAccount } from '@gridfpv/types';
  import type { PeopleApi } from '../lib/serverPeople.js';

  let {
    api,
    club,
    runs,
    members,
    me,
    onchange
  }: {
    api: Pick<PeopleApi, 'clubPilots' | 'giveTo' | 'freePilot' | 'refuseAsk'>;
    club: { id: string; name: string };
    /** Whether the person runs the club (a director or an owner), or only answers claims. */
    runs: boolean;
    /** The club's members, to give a pilot to. Only someone who runs the club has them. */
    members?: readonly MemberView[];
    /** The signed-in account's id. */
    me: string;
    /** Who holds what changed. */
    onchange?: () => void;
  } = $props();

  let pilots = $state<ClubPilotView[] | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  /** The pilot about to be taken from its holder. */
  let taking = $state<ClubPilotView | undefined>(undefined);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  async function load() {
    try {
      pilots = await api.clubPilots(clubId);
      loadError = undefined;
    } catch (err) {
      loadError = say(err);
    }
  }
  /**
   * The club by its id alone. The page hands over the club afresh each time it reads its
   * lists, and that is not another club: nothing here is thrown away for it.
   */
  const clubId = $derived(club.id);
  $effect(() => {
    void clubId;
    pilots = undefined;
    void load();
  });

  async function run(what: () => Promise<unknown>) {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      await what();
      onchange?.();
    } catch (err) {
      error = say(err);
    } finally {
      // What is shown must be what is so, whether it worked or not.
      await load();
      busy = false;
    }
  }

  /** Every ask, oldest pilots first: who asks, and for which pilot. */
  const asks = $derived(
    (pilots ?? []).flatMap((pilot) => pilot.asks.map((account) => ({ pilot, account })))
  );

  /** Give the pilot to the member picked, and put the box back to "Give to…". */
  async function give(pilot: ClubPilotView, box: HTMLSelectElement) {
    const account = box.value;
    box.value = '';
    if (account !== '') await run(() => api.giveTo(club.id, pilot.id, account));
  }

  async function take() {
    const target = taking;
    if (!target) return;
    await run(() => api.freePilot(club.id, target.id));
    taking = undefined;
  }

  const answer = (pilot: ClubPilotView, account: PushAccount, yes: boolean) =>
    run(() =>
      yes ? api.giveTo(club.id, pilot.id, account.id) : api.refuseAsk(club.id, pilot.id, account.id)
    );
</script>

<Card elevation="sm">
  <div class="block" data-testid="club-pilots">
    <div class="head">
      <h2>Pilots</h2>
      <InfoTip
        text="The account that holds a pilot gets the results flown under that pilot's name."
        label="About pilots"
      />
    </div>
    {#if loadError}
      <Banner tone="danger" title="This club's pilots could not be read.">{loadError}</Banner>
    {/if}
    {#if error}<Banner tone="danger"><span data-testid="pilots-error">{error}</span></Banner>{/if}

    {#if pilots === undefined}
      {#if !loadError}<p class="muted" role="status">Loading…</p>{/if}
    {:else if pilots.length === 0}
      <p class="muted">No pilots yet.</p>
    {:else}
      {#if asks.length > 0}
        <h3>Waiting for an answer</h3>
        <ul aria-label="Asks">
          {#each asks as { pilot, account } (pilot.id + ':' + account.id)}
            <li data-testid="pilot-ask">
              <span class="who"
                ><strong>{account.name}</strong> says they are
                <strong>{pilot.callsign}</strong></span
              >
              {#if !runs && account.id === me}
                <span class="muted">Someone else must answer your own ask.</span>
              {:else}
                <Button
                  size="sm"
                  variant="primary"
                  disabled={busy}
                  aria-label="Yes, {account.name} is {pilot.callsign}"
                  onclick={() => answer(pilot, account, true)}>Yes</Button
                >
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  aria-label="No, {account.name} is not {pilot.callsign}"
                  onclick={() => answer(pilot, account, false)}>No</Button
                >
              {/if}
            </li>
          {/each}
        </ul>
      {/if}

      <ul aria-label="Pilots">
        {#each pilots as pilot (pilot.id)}
          <li data-testid="club-pilot">
            <span class="who">{pilot.callsign}</span>
            {#if pilot.holder}
              <span class="muted" data-testid="pilot-holder">{pilot.holder.name}</span>
            {:else}
              <span class="muted" data-testid="pilot-holder">Nobody yet</span>
            {/if}
            {#if runs}
              {#if members && members.length > 0}
                <select
                  aria-label="Give {pilot.callsign} to"
                  disabled={busy}
                  onchange={(e) => give(pilot, e.currentTarget)}
                >
                  <option value="">Give to…</option>
                  {#each members as member (member.id)}
                    {#if member.id !== pilot.holder?.id}
                      <option value={member.id}>{member.name}</option>
                    {/if}
                  {/each}
                </select>
              {/if}
              {#if pilot.holder}
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  aria-label="Take {pilot.callsign} back from {pilot.holder.name}"
                  onclick={() => (taking = pilot)}>Take back</Button
                >
              {/if}
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  </div>
</Card>

{#if taking}
  {@const target = taking}
  <Dialog
    open={true}
    title="Take {target.callsign} back from {target.holder?.name ?? 'its holder'}?"
    onclose={() => (taking = undefined)}
  >
    <p>
      They stay in {club.name}. {target.callsign}'s results stop showing on their account.
    </p>
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (taking = undefined)}>Cancel</Button>
      <Button variant="danger" disabled={busy} onclick={take}>Take back</Button>
    {/snippet}
  </Dialog>
{/if}

<style>
  .head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .block {
    display: grid;
    gap: var(--gf-space-3);
  }
  h2 {
    margin: 0;
    font-size: var(--gf-font-size-lg);
  }
  h3 {
    margin: 0;
    font-size: var(--gf-font-size-md);
  }
  p {
    margin: 0;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  li {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .who {
    flex: 1;
    min-width: 8rem;
    overflow-wrap: anywhere;
  }
  li[data-testid='club-pilot'] .who {
    font-weight: var(--gf-font-weight-semibold);
  }
  select {
    font: inherit;
    color: var(--gf-text);
    background: var(--gf-surface-sunken);
    border: 1px solid var(--gf-border-strong);
    border-radius: var(--gf-radius-sm);
    padding: var(--gf-space-1) var(--gf-space-2);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-weight: normal;
  }
</style>
