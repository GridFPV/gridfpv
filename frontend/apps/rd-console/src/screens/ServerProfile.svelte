<script lang="ts">
  /**
   * ServerProfile — a pilot's own profile on a remote server (docs/primitives.md §13.7):
   * the callsign and colour they race under, and the ids they have elsewhere.
   *
   * It belongs to the account, so it is the same in every club on the server. Every part
   * may be left empty.
   */
  import { Button, Card, InfoTip, Input } from '@gridfpv/components';
  import type { Me, MyAccount } from '@gridfpv/types';
  import {
    profileChanged,
    profileForm,
    profileOf,
    profileProblem,
    type PeopleApi,
    type ProfileForm
  } from '../lib/serverPeople.js';

  let {
    api,
    account,
    onchange
  }: {
    api: Pick<PeopleApi, 'setProfile'>;
    account: MyAccount;
    /** The account changed: here it is as it now stands. */
    onchange: (me: Me) => void;
  } = $props();

  let form = $state<ProfileForm | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);

  const profile = $derived(account.profile ?? {});
  const empty = $derived(Object.keys(profileOf(profileForm(profile))).length === 0);
  const problem = $derived(form ? profileProblem(form) : undefined);
  const dirty = $derived(form !== undefined && profileChanged(form, profile));

  async function save(e: Event) {
    e.preventDefault();
    if (!form || problem || busy) return;
    busy = true;
    error = undefined;
    try {
      onchange(await api.setProfile(profileOf(form)));
      form = undefined;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }
</script>

<Card elevation="sm">
  <div class="block" data-testid="pilot-profile">
    <div class="row">
      <h2>Pilot profile</h2>
      <InfoTip
        text="How you are shown as a pilot, in every club on this server. The ids let a race director match you to MultiGP and Velocidrone."
        label="About the pilot profile"
      />
      <span class="grow"></span>
      {#if !form}
        <Button
          size="sm"
          variant="ghost"
          onclick={() => ((error = undefined), (form = profileForm(profile)))}
          >{empty ? 'Fill it in' : 'Change'}</Button
        >
      {/if}
    </div>

    {#if form}
      <!-- A refusal is about what was in the form then. Typing again clears it. -->
      <form onsubmit={save} oninput={() => (error = undefined)} aria-label="Pilot profile">
        <div class="fields">
          <label for="profile-callsign">Callsign</label>
          <Input
            id="profile-callsign"
            maxlength={40}
            autocomplete="off"
            placeholder={account.name}
            bind:value={form.callsign}
            disabled={busy}
          />
          <label for="profile-color">Colour</label>
          <span class="colour">
            <Input
              id="profile-color"
              maxlength={7}
              autocomplete="off"
              spellcheck="false"
              placeholder="#FF8800"
              bind:value={form.color}
              disabled={busy}
            />
            <input
              type="color"
              aria-label="Pick a colour"
              value={/^#[0-9a-fA-F]{6}$/.test(form.color.trim()) ? form.color.trim() : '#888888'}
              oninput={(e) => form && (form.color = e.currentTarget.value.toUpperCase())}
              disabled={busy}
            />
          </span>
          <label for="profile-multigp">MultiGP id</label>
          <Input
            id="profile-multigp"
            maxlength={40}
            autocomplete="off"
            spellcheck="false"
            bind:value={form.multigp_id}
            disabled={busy}
          />
          <label for="profile-velocidrone">Velocidrone id</label>
          <Input
            id="profile-velocidrone"
            maxlength={40}
            autocomplete="off"
            spellcheck="false"
            bind:value={form.velocidrone_id}
            disabled={busy}
          />
        </div>
        {#if problem}<p class="error" role="alert">{problem}</p>{/if}
        {#if error}<p class="error" role="alert" data-testid="profile-error">{error}</p>{/if}
        <div class="actions">
          <Button
            variant="primary"
            size="sm"
            type="submit"
            disabled={busy || !dirty || problem !== undefined}>Save</Button
          >
          <Button variant="ghost" size="sm" disabled={busy} onclick={() => (form = undefined)}
            >Cancel</Button
          >
        </div>
      </form>
    {:else if empty}
      <p class="muted" data-testid="profile-empty">Shown as {account.name}.</p>
    {:else}
      <dl>
        <dt>Callsign</dt>
        <dd data-testid="profile-callsign">
          {profile.callsign ?? `Not set. You are shown as ${account.name}.`}
        </dd>
        <dt>Colour</dt>
        <dd data-testid="profile-color">
          {#if profile.color}
            <span class="swatch" style:background={profile.color} aria-hidden="true"></span>
            {profile.color}
          {:else}Not set{/if}
        </dd>
        <dt>MultiGP id</dt>
        <dd data-testid="profile-multigp">{profile.multigp_id ?? 'Not set'}</dd>
        <dt>Velocidrone id</dt>
        <dd data-testid="profile-velocidrone">{profile.velocidrone_id ?? 'Not set'}</dd>
      </dl>
    {/if}
  </div>
</Card>

<style>
  .block,
  form {
    display: grid;
    gap: var(--gf-space-3);
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
  }
  .grow {
    flex: 1;
  }
  h2 {
    margin: 0;
    font-size: var(--gf-font-size-lg);
  }
  p {
    margin: 0;
  }
  .fields {
    display: grid;
    grid-template-columns: max-content minmax(0, 20rem);
    align-items: center;
    gap: var(--gf-space-2) var(--gf-space-3);
    font-size: var(--gf-font-size-sm);
  }
  .fields label,
  dt {
    color: var(--gf-text-muted);
  }
  .colour {
    display: flex;
    gap: var(--gf-space-2);
    align-items: center;
  }
  .colour input[type='color'] {
    width: 2.25rem;
    height: 2.25rem;
    padding: 0;
    border: 1px solid var(--gf-border-strong);
    border-radius: var(--gf-radius-sm);
    background: none;
  }
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: var(--gf-space-1) var(--gf-space-3);
    margin: 0;
    font-size: var(--gf-font-size-sm);
  }
  dd {
    margin: 0;
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .swatch {
    width: 1rem;
    height: 1rem;
    border-radius: 50%;
    border: 1px solid var(--gf-border-strong);
  }
  .actions {
    display: flex;
    gap: var(--gf-space-2);
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .error {
    color: var(--gf-danger);
    font-size: var(--gf-font-size-sm);
  }
</style>
