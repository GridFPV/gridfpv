<script lang="ts">
  /**
   * ServerClubJoin — who can join a club, for those who run it (docs/primitives.md §13.7).
   *
   * A club is **public** (anyone with an account joins) or **private** (a person asks, and
   * someone who runs the club says yes). Either way a director can add someone they find
   * by name, and invites work as before.
   */
  import { Badge, Banner, Button, Card, Input } from '@gridfpv/components';
  import type { PersonView } from '@gridfpv/types';
  import { personLabel, type PeopleApi } from '../lib/serverPeople.js';

  let {
    api,
    club,
    onchange
  }: {
    api: Pick<PeopleApi, 'setClubOpen' | 'joinAsks' | 'answerJoinAsk' | 'findPeople' | 'addMember'>;
    club: { id: string; name: string; open?: boolean; archived?: boolean };
    /** Told when the club's members, or whether it is public, changed. */
    onchange?: () => void | Promise<void>;
  } = $props();

  /** Whether the club is public, as last heard. It starts as the page was given it. */
  let open = $state<boolean | undefined>(undefined);
  const isOpen = $derived(open ?? club.open ?? false);

  let asks = $state<PersonView[] | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);

  let text = $state('');
  /** Who the last look found. `undefined` before any look. */
  let found = $state<PersonView[] | undefined>(undefined);
  /** What was typed for the look whose answer is shown. */
  let lookedFor = $state('');

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  async function load() {
    try {
      asks = await api.joinAsks(club.id);
      loadError = undefined;
    } catch (err) {
      loadError = say(err);
    }
  }
  $effect(() => {
    void club.id;
    void load();
  });

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
      busy = false;
    }
  }

  async function setOpen(next: boolean) {
    await run(async () => {
      open = (await api.setClubOpen(club.id, next)).open;
      await onchange?.();
    });
  }

  async function answer(person: PersonView, yes: boolean) {
    await run(() => api.answerJoinAsk(club.id, person.id, yes));
    // Whatever was said, the list is read again: it may have changed under the page.
    await load();
    if (yes) await onchange?.();
  }

  async function look(e: Event) {
    e.preventDefault();
    const wanted = text.trim();
    if (wanted.length < 2) return;
    await run(async () => {
      found = await api.findPeople(club.id, wanted);
      lookedFor = wanted;
    });
  }

  async function add(person: PersonView) {
    const added = await run(() => api.addMember(club.id, person.id));
    if (!added) return;
    found = found?.filter((other) => other.id !== person.id);
    await load();
    await onchange?.();
  }
</script>

<Card elevation="sm">
  <div class="block" data-testid="club-join-setting">
    <div class="row">
      <h2>Who can join</h2>
      <Badge tone={isOpen ? 'info' : 'neutral'}>
        <span data-testid="club-open-state">{isOpen ? 'Public' : 'Private'}</span>
      </Badge>
    </div>
    {#if error}<Banner tone="danger">{error}</Banner>{/if}
    {#if isOpen}
      <p>Anyone with an account on this server can join {club.name}, as a pilot.</p>
      <div>
        <Button size="sm" disabled={busy || club.archived} onclick={() => setOpen(false)}
          >Make it private</Button
        >
      </div>
    {:else}
      <p>
        People ask to join {club.name}, and you say yes or no. You can also invite them, or add them
        below.
      </p>
      <div>
        <Button size="sm" disabled={busy || club.archived} onclick={() => setOpen(true)}
          >Make it public</Button
        >
      </div>
    {/if}

    <h3>Asked to join</h3>
    {#if loadError}
      <p class="muted">{loadError}</p>
    {:else if asks === undefined}
      <p class="muted" role="status">Loading…</p>
    {:else if asks.length === 0}
      <p class="muted" data-testid="no-join-asks">Nobody is waiting.</p>
    {:else}
      <ul aria-label="People who asked to join">
        {#each asks as person (person.id)}
          <li data-testid="join-ask">
            <span class="name">{personLabel(person)}</span>
            <span class="grow"></span>
            <Button
              size="sm"
              variant="primary"
              disabled={busy || club.archived}
              aria-label="Let {personLabel(person)} in"
              onclick={() => answer(person, true)}>Let in</Button
            >
            <Button
              size="sm"
              variant="ghost"
              disabled={busy || club.archived}
              aria-label="Say no to {personLabel(person)}"
              onclick={() => answer(person, false)}>No</Button
            >
          </li>
        {/each}
      </ul>
    {/if}

    <h3>Add someone</h3>
    <p class="muted">
      Find someone who has an account on this server, by their name or callsign. They join as a
      pilot.
    </p>
    <form class="row" onsubmit={look} aria-label="Find someone to add">
      <label class="visually-hidden" for="find-person-{club.id}">Name or callsign</label>
      <Input
        id="find-person-{club.id}"
        maxlength={80}
        autocomplete="off"
        placeholder="Name or callsign"
        bind:value={text}
        disabled={busy || club.archived}
      />
      <Button size="sm" type="submit" disabled={busy || club.archived || text.trim().length < 2}
        >Find</Button
      >
    </form>
    {#if found !== undefined}
      {#if found.length === 0}
        <p class="muted" data-testid="nobody-found">
          Nobody outside the club matches “{lookedFor}”.
        </p>
      {:else}
        <ul aria-label="People found">
          {#each found as person (person.id)}
            <li data-testid="found-person">
              <span class="name">{personLabel(person)}</span>
              <span class="grow"></span>
              <Button
                size="sm"
                disabled={busy || club.archived}
                aria-label="Add {personLabel(person)} to {club.name}"
                onclick={() => add(person)}>Add</Button
              >
            </li>
          {/each}
        </ul>
      {/if}
    {/if}
  </div>
</Card>

<style>
  .block {
    display: grid;
    gap: var(--gf-space-3);
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
  }
  h2 {
    flex: 1;
    margin: 0;
    font-size: var(--gf-font-size-lg);
  }
  h3 {
    margin: var(--gf-space-2) 0 0;
    font-size: var(--gf-font-size-md);
  }
  p {
    margin: 0;
    max-width: 65ch;
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
    gap: var(--gf-space-2);
  }
  .grow {
    flex: 1;
  }
  .name {
    font-weight: var(--gf-font-weight-semibold);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .visually-hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
