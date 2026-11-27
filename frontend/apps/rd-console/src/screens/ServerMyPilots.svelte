<script lang="ts">
  /**
   * ServerMyPilots — the pilots the signed-in account holds, and the way to ask for one
   * (docs/primitives.md §13.7).
   *
   * A club's director can put a pilot on an account, and a personal invite can carry
   * one. Otherwise the person finds themselves in their club's list and asks. Asking gives
   * nothing: a director or a claims approver of the club must say yes.
   */
  import { Banner, Button, Card, Dialog, InfoTip } from '@gridfpv/components';
  import type { ClubPilotView, MyAccount, MyPilot } from '@gridfpv/types';
  import { formatServerHash } from '../lib/serverHome.js';
  import type { PeopleApi } from '../lib/serverPeople.js';

  let {
    api,
    account,
    onchange
  }: {
    api: Pick<PeopleApi, 'clubPilots' | 'askFor' | 'dropClaim'>;
    account: MyAccount;
    /** What the account holds or asks for changed: read it again. */
    onchange: () => void | Promise<void>;
  } = $props();

  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  /** The club whose list is open, to find a pilot in. */
  let looking = $state<{ id: string; name: string } | undefined>(undefined);
  /** That club's pilots nobody holds. */
  let free = $state<ClubPilotView[] | undefined>(undefined);
  let lookError = $state<string | undefined>(undefined);
  /** The held pilot about to be let go. */
  let letting = $state<MyPilot | undefined>(undefined);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));
  const held = $derived(account.pilots.some((pilot) => pilot.held));

  async function run(what: () => Promise<unknown>): Promise<boolean> {
    if (busy) return false;
    busy = true;
    error = undefined;
    try {
      await what();
      return true;
    } catch (err) {
      error = say(err);
      return false;
    } finally {
      await onchange();
      busy = false;
    }
  }

  async function look(club: { id: string; name: string }) {
    looking = club;
    free = undefined;
    lookError = undefined;
    try {
      const all = await api.clubPilots(club.id);
      free = all.filter((pilot) => !pilot.taken && !pilot.asked);
    } catch (err) {
      lookError = say(err);
    }
  }

  async function ask(pilot: ClubPilotView) {
    const club = looking;
    if (!club) return;
    looking = undefined;
    await run(() => api.askFor(club.id, pilot.id));
  }

  async function letGo() {
    const target = letting;
    if (!target) return;
    letting = undefined;
    await run(() => api.dropClaim(target.club.id, target.id));
  }
</script>

<Card elevation="sm">
  <div class="block" data-testid="my-pilots">
    <div class="head">
      <h2>Your pilots</h2>
      <InfoTip
        text="Find yourself in your club's list. The results flown under that pilot then show on your account."
        label="About your pilots"
      />
    </div>
    {#if error}<Banner tone="danger">{error}</Banner>{/if}
    {#if account.pilots.length === 0}
      <p class="muted">None yet.</p>
    {:else}
      <ul aria-label="Your pilots">
        {#each account.pilots as pilot (pilot.club.id + ':' + pilot.id)}
          <li data-testid="my-pilot">
            <span class="who">{pilot.callsign}</span>
            <span class="muted">{pilot.club.name}</span>
            {#if pilot.held}
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                aria-label="Let go of {pilot.callsign}"
                onclick={() => (letting = pilot)}>Let go</Button
              >
            {:else}
              <span class="muted">Waiting for the club to say yes</span>
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                aria-label="Stop asking for {pilot.callsign}"
                onclick={() => run(() => api.dropClaim(pilot.club.id, pilot.id))}
                >Stop asking</Button
              >
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
    {#if held}
      <p><a href={formatServerHash({ kind: 'results' })}>Your results</a></p>
    {/if}
    {#if account.clubs.length > 0}
      <div class="row">
        {#each account.clubs as entry (entry.club.id)}
          <Button size="sm" disabled={busy} onclick={() => look(entry.club)}
            >Find yourself in {entry.club.name}</Button
          >
        {/each}
      </div>
    {/if}
  </div>
</Card>

{#if looking}
  {@const club = looking}
  <Dialog
    open={true}
    title="Which pilot are you in {club.name}?"
    onclose={() => (looking = undefined)}
  >
    {#if lookError}
      <Banner tone="danger">{lookError}</Banner>
    {:else if free === undefined}
      <p class="muted" role="status">Loading…</p>
    {:else if free.length === 0}
      <p>
        There is no pilot here to ask for. A pilot shows up once you have flown in one of the club's
        events.
      </p>
    {:else}
      <p class="muted">A director or a claims approver of the club will be asked to say yes.</p>
      <ul aria-label="Pilots of {club.name}">
        {#each free as pilot (pilot.id)}
          <li>
            <span class="who">{pilot.callsign}</span>
            <Button size="sm" aria-label="I am {pilot.callsign}" onclick={() => ask(pilot)}
              >This is me</Button
            >
          </li>
        {/each}
      </ul>
    {/if}
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (looking = undefined)}>Close</Button>
    {/snippet}
  </Dialog>
{/if}

{#if letting}
  {@const target = letting}
  <Dialog open={true} title="Let go of {target.callsign}?" onclose={() => (letting = undefined)}>
    <p>
      {target.callsign}'s results in {target.club.name} stop showing on your account. To get them back
      you ask the club again.
    </p>
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (letting = undefined)}>Cancel</Button>
      <Button variant="danger" disabled={busy} onclick={letGo}>Let go</Button>
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
    font-weight: var(--gf-font-weight-semibold);
  }
  .row {
    display: flex;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
</style>
