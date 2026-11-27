<script lang="ts">
  /**
   * ServerMemberSignIns — an owner of the server helping someone who can no longer sign in
   * (docs/primitives.md §13.8).
   *
   * There are no passwords, so there is nothing to reset. The owner can take a sign-in off
   * the person's account (a stolen or closed one), and make a re-connect link: the person
   * opens it, signs in with an account they do have, and is back in their old account.
   *
   * The link is shown once, when it is made. It is as good as the account, so the dialog
   * says to send it only to the person.
   */
  import { Banner, Button, Dialog, toast } from '@gridfpv/components';
  import type { AccountSignIns, MySignIn, ReconnectMade, SignInProvider } from '@gridfpv/types';
  import { providerName, reconnectLink, untilDay, type PeopleApi } from '../lib/serverPeople.js';

  let {
    api,
    member,
    me,
    providers,
    origin = location.origin,
    onclose
  }: {
    api: Pick<PeopleApi, 'accountSignIns' | 'dropSignIn' | 'makeReconnect' | 'revokeReconnect'>;
    member: { id: string; name: string };
    /** The signed-in owner's own account id: their last sign-in stays. */
    me: string;
    providers: readonly SignInProvider[] | undefined;
    /** This server's address, for the link shown. A test passes its own. */
    origin?: string;
    onclose: () => void;
  } = $props();

  let held = $state<AccountSignIns | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  /** The sign-in about to be taken off. */
  let dropping = $state<MySignIn | undefined>(undefined);
  /** The link just made. It is not shown again. */
  let made = $state<ReconnectMade | undefined>(undefined);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));
  const named = (key: MySignIn): string =>
    [providerName(key.provider, providers ?? []), key.label]
      .filter((part) => part !== '')
      .join(' ');

  $effect(() => {
    const asked = member.id;
    held = undefined;
    made = undefined;
    dropping = undefined;
    error = undefined;
    let stopped = false;
    api
      .accountSignIns(asked)
      .then((found) => {
        if (!stopped) held = found;
      })
      .catch((err) => {
        if (!stopped) error = say(err);
      });
    return () => {
      stopped = true;
    };
  });

  async function run(what: () => Promise<void>) {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      await what();
    } catch (err) {
      error = say(err);
    } finally {
      busy = false;
    }
  }

  const drop = () =>
    run(async () => {
      const key = dropping;
      if (!key) return;
      dropping = undefined;
      held = await api.dropSignIn(member.id, key.provider, key.subject);
    });

  const make = () =>
    run(async () => {
      made = await api.makeReconnect(member.id);
      held = await api.accountSignIns(member.id);
    });

  const takeBack = () =>
    run(async () => {
      await api.revokeReconnect(member.id);
      made = undefined;
      held = await api.accountSignIns(member.id);
    });

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      toast.success('Copied.');
    } catch {
      toast.error('Could not copy. Select the link and copy it by hand.');
    }
  }
</script>

<Dialog open={true} title="How {member.name} signs in" {onclose}>
  <div class="body" data-testid="member-sign-ins">
    {#if error}<Banner tone="danger"><span data-testid="sign-ins-error">{error}</span></Banner>{/if}
    {#if !held}
      {#if !error}<p class="muted" role="status">Loading…</p>{/if}
    {:else if dropping}
      {@const key = dropping}
      <p data-testid="drop-sure">
        Take <strong>{named(key)}</strong> off {member.name}'s account?
      </p>
      <p>
        {member.name} is signed out everywhere, and that sign-in no longer opens this account.
        {#if held.sign_ins.length === 1}
          It is their only one, so they cannot sign in until you send them a re-connect link.
        {/if}
      </p>
      <div class="row">
        <Button variant="ghost" disabled={busy} onclick={() => (dropping = undefined)}
          >Cancel</Button
        >
        <Button variant="danger" disabled={busy} onclick={drop}>Take it off</Button>
      </div>
    {:else}
      {#if held.sign_ins.length === 0}
        <p data-testid="no-sign-ins">
          {member.name} has no way to sign in. Send them a re-connect link.
        </p>
      {:else}
        <ul aria-label="How {member.name} signs in">
          {#each held.sign_ins as key (key.provider + ':' + key.subject)}
            <li data-testid="member-sign-in">
              <span>{providerName(key.provider, providers ?? [])}</span>
              <span class="muted">{key.label}</span>
              {#if member.id !== me || held.sign_ins.length > 1}
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  aria-label="Take off {named(key)}"
                  onclick={() => (dropping = key)}>Take off</Button
                >
              {/if}
            </li>
          {/each}
        </ul>
      {/if}

      <h3>Re-connect link</h3>
      <p>
        A link that lets {member.name} sign in with another account and get back in to this one. Their
        clubs, pilots and results stay as they are.
      </p>
      <p class="muted">
        It works once, and for seven days. Whoever opens it gets into the account, so send it only
        to {member.name}.
      </p>
      {#if made}
        {@const link = reconnectLink(origin, made.secret)}
        <div class="made" data-testid="reconnect-made">
          <p>Send this link to <strong>{member.name}</strong>. It is shown this once.</p>
          <code data-testid="reconnect-link">{link}</code>
          <div class="row">
            <Button size="sm" onclick={() => copy(link)}>Copy</Button>
          </div>
        </div>
      {/if}
      {#if held.link_until !== undefined}
        <p data-testid="reconnect-waiting">
          A link is waiting, {untilDay(held.link_until)}.
        </p>
      {/if}
      <div class="row">
        <Button size="sm" variant="primary" disabled={busy} onclick={make}
          >{held.link_until !== undefined ? 'Make a new link' : 'Make a re-connect link'}</Button
        >
        {#if held.link_until !== undefined}
          <Button size="sm" variant="ghost" disabled={busy} onclick={takeBack}
            >Take back the link</Button
          >
        {/if}
      </div>
      {#if held.link_until !== undefined && !made}
        <p class="muted">A new link stops the one that is waiting.</p>
      {/if}
    {/if}
  </div>
  {#snippet footer()}
    <Button variant="ghost" onclick={onclose}>Done</Button>
  {/snippet}
</Dialog>

<style>
  .body {
    display: grid;
    gap: var(--gf-space-3);
  }
  p,
  h3 {
    margin: 0;
  }
  h3 {
    font-size: var(--gf-font-size-md);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  li,
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--gf-space-2);
  }
  li span:first-child {
    font-weight: var(--gf-font-weight-bold);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .made {
    display: grid;
    gap: var(--gf-space-2);
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
  }
  code {
    overflow-wrap: anywhere;
    user-select: all;
  }
</style>
