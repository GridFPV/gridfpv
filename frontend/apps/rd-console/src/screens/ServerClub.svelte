<script lang="ts">
  /**
   * ServerClub — a club's members, invites and pilots, for those who look after it
   * (docs/primitives.md §13.2, §13.7, §13.8).
   *
   * Someone who runs the club (a director, or a server owner) sees all three, and below
   * them the club's events, link tokens and name (`ServerClubManage`). A claims
   * approver sees only the pilots, to answer the members who asked for one.
   *
   * Only a club's directors hand out its roles and invite to it. A club always keeps at
   * least one director: the server refuses the change that would leave it with none, and
   * its words are shown.
   *
   * An invite's link is shown once, when it is made. After that the list has only what it
   * is for and how it stands.
   */
  import { Banner, Button, Card, Dialog, Input, toast } from '@gridfpv/components';
  import type {
    ClubPilotView,
    ClubRole,
    InviteFor,
    InviteMade,
    InviteView,
    MemberView,
    SignInProvider
  } from '@gridfpv/types';
  import {
    ROLES,
    inviteLink,
    inviteStanding,
    roleWord,
    type PeopleApi
  } from '../lib/serverPeople.js';
  import ServerClubManage from './ServerClubManage.svelte';
  import ServerClubPilots from './ServerClubPilots.svelte';
  import ServerMemberSignIns from './ServerMemberSignIns.svelte';

  let {
    api,
    club,
    me,
    runs = true,
    owner = false,
    providers = undefined,
    origin = location.origin,
    onchange
  }: {
    /** Told when the club's name or its list of events changed. */
    onchange?: () => void;
    api: Pick<
      PeopleApi,
      | 'members'
      | 'setRole'
      | 'removeMember'
      | 'clubInvites'
      | 'makeInvite'
      | 'revokeInvite'
      | 'clubPilots'
      | 'giveTo'
      | 'freePilot'
      | 'refuseAsk'
      | 'renameClub'
      | 'linkTokens'
      | 'makeLinkToken'
      | 'revokeLinkToken'
      | 'clubEvents'
      | 'hideEvent'
      | 'removeEvent'
      | 'accountSignIns'
      | 'dropSignIn'
      | 'makeReconnect'
      | 'revokeReconnect'
    >;
    club: { id: string; name: string };
    /** The signed-in account's id, to mark its own row. */
    me: string;
    /** Whether the person runs the club. A claims approver does not: they see only pilots. */
    runs?: boolean;
    /**
     * Whether the person is an owner of the server. Only an owner helps a member who can
     * no longer sign in (§13.8).
     */
    owner?: boolean;
    /** The providers the server offers, to name a sign-in by. */
    providers?: readonly SignInProvider[] | undefined;
    /** This server's address, for the link shown. A test passes its own. */
    origin?: string;
  } = $props();

  let members = $state<MemberView[] | undefined>(undefined);
  let invites = $state<InviteView[] | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  /** The member about to be removed. */
  let removing = $state<MemberView | undefined>(undefined);
  /** The member whose sign-ins an owner is looking at. */
  let helping = $state<MemberView | undefined>(undefined);
  /** The invite just made, with its link. It is not shown again. */
  let made = $state<InviteMade | undefined>(undefined);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  /** The club's pilots nobody holds, for an invite made for one of them. */
  let freePilots = $state<ClubPilotView[]>([]);

  async function load() {
    // Only someone who runs the club may read its members and invites.
    if (!runs) return;
    try {
      [members, invites] = await Promise.all([api.members(club.id), api.clubInvites(club.id)]);
      loadError = undefined;
      // The invite form offers these. Without them an invite is still made, for no pilot.
      freePilots = await api
        .clubPilots(club.id)
        .then((all) => all.filter((pilot) => !pilot.taken))
        .catch(() => []);
    } catch (err) {
      loadError = say(err);
    }
  }
  $effect(() => {
    void club.id;
    void runs;
    members = undefined;
    invites = undefined;
    made = undefined;
    void load();
  });

  async function run(what: () => Promise<unknown>): Promise<boolean> {
    if (busy) return false;
    busy = true;
    error = undefined;
    try {
      await what();
      await load();
      return true;
    } catch (err) {
      error = say(err);
      // The list may be out of date: that is often why it was refused.
      await load();
      return false;
    } finally {
      busy = false;
    }
  }

  /**
   * Give a member the role picked. When the server refuses, the box goes back to the role
   * they still have: it must never show a role that was not given.
   */
  async function changeRole(member: MemberView, box: HTMLSelectElement) {
    const ok = await run(() => api.setRole(club.id, member.id, box.value as ClubRole));
    if (!ok) box.value = members?.find((m) => m.id === member.id)?.role ?? member.role;
  }

  async function remove() {
    const target = removing;
    if (!target) return;
    await run(() => api.removeMember(club.id, target.id));
    removing = undefined;
  }

  // ── A new invite ───────────────────────────────────────────────────────────
  let kind = $state<InviteFor>('Personal');
  let label = $state('');
  let role = $state<ClubRole>('Pilot');
  /** The club's pilot a personal invite is for. Empty for none. */
  let pilot = $state('');
  let uses = $state(20);
  let days = $state(7);
  const canMake = $derived(
    label.trim() !== '' &&
      (kind === 'Personal' || (uses >= 1 && uses <= 500 && days >= 1 && days <= 90))
  );

  async function make(e: Event) {
    e.preventDefault();
    if (!canMake) return;
    const request =
      kind === 'Personal'
        ? { kind, label: label.trim(), role, ...(pilot !== '' ? { pilot } : {}) }
        : { kind, label: label.trim(), max_uses: uses, days };
    let result: InviteMade | undefined;
    if (
      await run(async () => {
        result = await api.makeInvite(club.id, request);
      })
    ) {
      made = result;
      label = '';
      pilot = '';
    }
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      toast.success('Copied.');
    } catch {
      toast.error('Could not copy. Select the link and copy it by hand.');
    }
  }

  const now = $derived(Date.now());
</script>

<div class="club" data-testid="server-club">
  <p class="lead">{club.name}</p>
  {#if loadError}
    <Banner tone="danger" title="This club's members could not be read.">{loadError}</Banner>
  {/if}
  {#if error}<Banner tone="danger"><span data-testid="club-error">{error}</span></Banner>{/if}

  {#if !runs}
    <ServerClubPilots {api} {club} {me} runs={false} />
  {:else}
    <Card elevation="sm">
      <div class="block">
        <h2>Members</h2>
        {#if members === undefined}
          {#if !loadError}<p class="muted" role="status">Loading…</p>{/if}
        {:else}
          <ul aria-label="Members">
            {#each members as member (member.id)}
              <li data-testid="club-member">
                <span class="who"
                  >{member.name}{#if member.id === me}<span class="muted"> (you)</span>{/if}</span
                >
                <select
                  aria-label="Role of {member.name}"
                  value={member.role}
                  disabled={busy}
                  onchange={(e) => changeRole(member, e.currentTarget)}
                >
                  {#each ROLES as option (option)}
                    <option value={option}>{roleWord(option)}</option>
                  {/each}
                </select>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  aria-label="Remove {member.name}"
                  onclick={() => (removing = member)}>Remove</Button
                >
                {#if owner}
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    aria-label="How {member.name} signs in"
                    onclick={() => (helping = member)}>Sign-ins…</Button
                  >
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    </Card>

    <Card elevation="sm">
      <div class="block">
        <h2>Invite someone</h2>
        <form onsubmit={make} aria-label="New invite">
          <div class="kinds" role="radiogroup" aria-label="Who the invite is for">
            <label><input type="radio" bind:group={kind} value="Personal" /> One person</label>
            <label
              ><input type="radio" bind:group={kind} value="Group" /> Anyone with the link</label
            >
          </div>
          <div class="fields">
            <label for="invite-label"
              >{kind === 'Personal' ? 'Who it is for' : 'Where you will post it'}</label
            >
            <Input
              id="invite-label"
              maxlength={80}
              placeholder={kind === 'Personal' ? 'Their callsign' : 'Club Discord'}
              bind:value={label}
              disabled={busy}
            />
            {#if kind === 'Personal'}
              <label for="invite-role">Joins as</label>
              <select id="invite-role" bind:value={role} disabled={busy}>
                {#each ROLES as option (option)}
                  <option value={option}>{roleWord(option)}</option>
                {/each}
              </select>
              {#if freePilots.length > 0}
                <label for="invite-pilot">Their pilot</label>
                <select id="invite-pilot" bind:value={pilot} disabled={busy}>
                  <option value="">Not one of these</option>
                  {#each freePilots as option (option.id)}
                    <option value={option.id}>{option.callsign}</option>
                  {/each}
                </select>
              {/if}
            {:else}
              <label for="invite-uses">How many people</label>
              <Input
                id="invite-uses"
                type="number"
                min={1}
                max={500}
                bind:value={uses}
                disabled={busy}
              />
              <label for="invite-days">Days it lasts</label>
              <Input
                id="invite-days"
                type="number"
                min={1}
                max={90}
                bind:value={days}
                disabled={busy}
              />
            {/if}
          </div>
          <div>
            <Button variant="primary" size="sm" type="submit" disabled={busy || !canMake}
              >Make the invite</Button
            >
          </div>
        </form>

        {#if made}
          {@const link = inviteLink(origin, made.secret)}
          <div class="made" data-testid="invite-made">
            <p>
              {#if made.invite.kind === 'Group'}
                Post this link in <strong>{made.invite.label}</strong>. It is shown this once.
              {:else}
                Send this link to <strong>{made.invite.label}</strong>. It is shown this once.
              {/if}
              {#if made.invite.pilot}
                Whoever uses it gets <strong>{made.invite.pilot.callsign}</strong>'s results.
              {/if}
            </p>
            <code data-testid="invite-link">{link}</code>
            <div class="row">
              <Button size="sm" onclick={() => copy(link)}>Copy</Button>
              <Button size="sm" variant="ghost" onclick={() => (made = undefined)}>Done</Button>
            </div>
          </div>
        {/if}

        {#if invites && invites.length > 0}
          <ul aria-label="Invites">
            {#each invites as invite (invite.id)}
              <li data-testid="club-invite" class:spent={!invite.works}>
                <span class="who">{invite.label}</span>
                <span class="muted"
                  >{invite.kind === 'Group'
                    ? 'Anyone with the link'
                    : roleWord(invite.role ?? 'Pilot')}{invite.pilot
                    ? `, as ${invite.pilot.callsign}`
                    : ''}</span
                >
                <span class="muted">{inviteStanding(invite, now)}</span>
                {#if invite.works}
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    aria-label="Take back the invite for {invite.label}"
                    onclick={() => run(() => api.revokeInvite(club.id, invite.id))}
                    >Take back</Button
                  >
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    </Card>

    <ServerClubPilots {api} {club} {me} runs {members} onchange={load} />
    <ServerClubManage {api} {club} {onchange} />
  {/if}
</div>

{#if removing}
  {@const target = removing}
  <Dialog
    open={true}
    title="Remove {target.name} from {club.name}?"
    onclose={() => (removing = undefined)}
  >
    <p>They keep their account, and their place in any other club.</p>
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (removing = undefined)}>Cancel</Button>
      <Button variant="danger" disabled={busy} onclick={remove}>Remove</Button>
    {/snippet}
  </Dialog>
{/if}

{#if helping}
  <ServerMemberSignIns
    {api}
    member={helping}
    {me}
    {providers}
    {origin}
    onclose={() => (helping = undefined)}
  />
{/if}

<style>
  .club,
  .block,
  form {
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
  .lead {
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-bold);
    overflow-wrap: anywhere;
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
  .spent .who {
    font-weight: normal;
    color: var(--gf-text-muted);
  }
  .kinds,
  .row {
    display: flex;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .fields {
    display: grid;
    grid-template-columns: max-content minmax(0, 20rem);
    align-items: center;
    gap: var(--gf-space-2) var(--gf-space-3);
    font-size: var(--gf-font-size-sm);
  }
  .made {
    display: grid;
    gap: var(--gf-space-2);
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-accent);
    border-radius: var(--gf-radius-sm);
  }
  code {
    font-family: var(--gf-font-mono);
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-2);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    overflow-wrap: anywhere;
    user-select: all;
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
