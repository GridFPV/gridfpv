<script lang="ts">
  /**
   * ServerEmailSetting — the email account a server sends from, for an owner of it
   * (docs/primitives.md §13.8): which email server, who the mail is from, and a test.
   *
   * The account's password is typed here and never shown again: the server only says
   * whether it has one. Settings in the server's own file still work. What is saved here
   * is used in their place.
   */
  import { Banner, Button, Card, Dialog, InfoTip, Input, Select } from '@gridfpv/components';
  import type { MailTls, ServerEmail, ServerEmailChange } from '@gridfpv/types';
  import type { PeopleApi } from '../lib/serverPeople.js';

  let {
    api,
    onchange
  }: {
    api: Pick<PeopleApi, 'mailSettings' | 'setMailSettings' | 'removeMailSettings' | 'testMail'>;
    /** Told whether the server sends email now. */
    onchange?: (on: boolean) => void;
  } = $props();

  let shown = $state<ServerEmail | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  let saved = $state(false);
  /** The owner is being asked whether to take the settings off. */
  let removing = $state(false);

  // What is typed.
  let host = $state('');
  let port = $state('');
  let tls = $state<MailTls>('starttls');
  let user = $state('');
  let password = $state('');
  let from = $state('');

  // The test.
  let to = $state('');
  let testing = $state(false);
  let testError = $state<string | undefined>(undefined);
  let sentTo = $state<string | undefined>(undefined);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  /** The port used when none is typed. */
  const usualPort = $derived(tls === 'tls' ? '465' : '587');
  const SOURCE: Record<ServerEmail['source'], string> = {
    page: 'On',
    env: 'On, from the settings file',
    off: 'Off'
  };

  function take(view: ServerEmail) {
    shown = view;
    host = view.host ?? '';
    port = view.port === undefined ? '' : String(view.port);
    tls = view.tls ?? 'starttls';
    user = view.user ?? '';
    from = view.from ?? '';
    password = '';
    onchange?.(view.source !== 'off' && view.knows_address);
  }

  $effect(() => {
    api
      .mailSettings()
      .then((view) => {
        take(view);
        loadError = undefined;
      })
      .catch((err) => (loadError = say(err)));
  });

  const portProblem = $derived.by(() => {
    const text = port.trim();
    if (text === '') return undefined;
    const n = Number(text);
    return Number.isInteger(n) && n >= 1 && n <= 65535
      ? undefined
      : 'A port is a number from 1 to 65535.';
  });
  const ready = $derived(host.trim() !== '' && from.trim() !== '' && portProblem === undefined);

  async function save(e: Event) {
    e.preventDefault();
    if (busy || !ready) return;
    busy = true;
    error = undefined;
    saved = false;
    sentTo = undefined;
    const change: ServerEmailChange = {
      host: host.trim(),
      from: from.trim(),
      tls,
      ...(port.trim() === '' ? {} : { port: Number(port.trim()) }),
      ...(user.trim() === '' ? {} : { user: user.trim() }),
      ...(password === '' ? {} : { password })
    };
    try {
      take(await api.setMailSettings(change));
      saved = true;
    } catch (err) {
      error = say(err);
    } finally {
      busy = false;
    }
  }

  async function remove() {
    if (busy) return;
    busy = true;
    error = undefined;
    saved = false;
    try {
      take(await api.removeMailSettings());
      removing = false;
    } catch (err) {
      error = say(err);
    } finally {
      busy = false;
    }
  }

  async function test(e: Event) {
    e.preventDefault();
    const address = to.trim();
    if (testing || address === '') return;
    testing = true;
    testError = undefined;
    sentTo = undefined;
    try {
      await api.testMail(address);
      sentTo = address;
    } catch (err) {
      testError = say(err);
    } finally {
      testing = false;
    }
  }
</script>

<div class="cards">
  <Card elevation="sm">
    <div class="block" data-testid="email-setting">
      <div class="row">
        <h2>Email</h2>
        <InfoTip
          text="The email account this server sends from. It is used to confirm an address, and to send a link for a forgotten password."
          label="About the server's email"
        />
        <span class="grow"></span>
        {#if shown}<span class="state" data-testid="email-on">{SOURCE[shown.source]}</span>{/if}
      </div>
      {#if loadError}<Banner tone="danger">{loadError}</Banner>{/if}
      {#if shown && !shown.knows_address}
        <Banner tone="warn" title="This server does not know its own address.">
          <span data-testid="email-no-address"
            >No mail is sent until GRIDFPV_SERVER_PUBLIC_URL is set.</span
          >
        </Banner>
      {/if}
      {#if shown}
        <!-- What was refused or saved was about what was in the form then. -->
        <form
          class="fields"
          onsubmit={save}
          oninput={() => ((error = undefined), (saved = false))}
          aria-label="Email settings"
        >
          <label for="mail-host">Server</label>
          <Input
            id="mail-host"
            maxlength={253}
            placeholder="smtp.example.org"
            autocapitalize="none"
            spellcheck="false"
            bind:value={host}
            disabled={busy}
          />
          <label for="mail-tls">Security</label>
          <Select id="mail-tls" bind:value={tls} disabled={busy}>
            <option value="starttls">STARTTLS</option>
            <option value="tls">TLS</option>
            <option value="none">None (this computer only)</option>
          </Select>
          <label for="mail-port">Port</label>
          <Input
            id="mail-port"
            inputmode="numeric"
            maxlength={5}
            placeholder={usualPort}
            bind:value={port}
            disabled={busy}
          />
          <label for="mail-user">Username</label>
          <Input
            id="mail-user"
            maxlength={254}
            autocomplete="off"
            autocapitalize="none"
            spellcheck="false"
            bind:value={user}
            disabled={busy}
          />
          <span class="labelled">
            <label for="mail-password">Password</label>
            <InfoTip
              text="Kept on the server and never shown again. Leave it empty to keep the one it has."
              label="About the email password"
            />
          </span>
          <Input
            id="mail-password"
            type="password"
            maxlength={500}
            autocomplete="new-password"
            placeholder={shown.password_set ? 'Kept' : ''}
            bind:value={password}
            disabled={busy}
          />
          <label for="mail-from">From</label>
          <Input
            id="mail-from"
            maxlength={300}
            placeholder="Grid <grid@example.org>"
            bind:value={from}
            disabled={busy}
          />
          {#if portProblem}<p class="error wide" role="alert">{portProblem}</p>{/if}
          {#if error}
            <p class="error wide" role="alert" data-testid="email-setting-error">{error}</p>
          {/if}
          <div class="actions wide">
            <Button variant="primary" size="sm" type="submit" disabled={busy || !ready}>Save</Button
            >
            {#if shown.source === 'page'}
              <Button
                variant="ghost"
                size="sm"
                disabled={busy}
                onclick={() => ((error = undefined), (removing = true))}>Remove…</Button
              >
            {/if}
            {#if saved}<span class="state" role="status" data-testid="email-saved">Saved</span>{/if}
          </div>
        </form>
      {/if}
    </div>
  </Card>

  {#if shown && shown.source !== 'off'}
    <Card elevation="sm">
      <div class="block" data-testid="email-test">
        <h2>Send a test</h2>
        <form
          class="row"
          onsubmit={test}
          oninput={() => (testError = undefined)}
          aria-label="Send a test"
        >
          <label class="visually-hidden" for="mail-test-to">Send a test to</label>
          <Input
            id="mail-test-to"
            type="email"
            maxlength={254}
            placeholder="you@example.org"
            bind:value={to}
            disabled={testing}
          />
          <Button size="sm" type="submit" disabled={testing || to.trim() === ''}
            >{testing ? 'Sending…' : 'Send'}</Button
          >
        </form>
        {#if testError}
          <Banner tone="danger" title="The test did not go.">
            <span data-testid="email-test-error">{testError}</span>
          </Banner>
        {/if}
        {#if sentTo}
          <p class="state" role="status" data-testid="email-test-sent">Sent to {sentTo}.</p>
        {/if}
      </div>
    </Card>
  {/if}
</div>

{#if removing}
  <Dialog open={true} title="Remove the email settings?" onclose={() => (removing = false)}>
    <p>Nobody can confirm an address or set a forgotten password by email until it is set again.</p>
    <p class="state">
      An email account in the server's settings file is used again, if it has one.
    </p>
    {#if error}<Banner tone="danger">{error}</Banner>{/if}
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (removing = false)}>Cancel</Button>
      <Button variant="danger" disabled={busy} onclick={remove}>Remove</Button>
    {/snippet}
  </Dialog>
{/if}

<style>
  .cards {
    display: grid;
    /* A form reads badly stretched across a wide screen. */
    grid-template-columns: repeat(auto-fit, minmax(min(24rem, 100%), 34rem));
    gap: var(--gf-space-4);
    align-items: start;
  }
  .block {
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
  /* A label beside each box on a wide card, above it on a narrow one. */
  .fields {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    align-items: center;
    gap: var(--gf-space-2) var(--gf-space-3);
  }
  .fields label {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .labelled {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .wide {
    grid-column: 1 / -1;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    margin-top: var(--gf-space-1);
  }
  .state {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .error {
    color: var(--gf-danger);
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
  @media (max-width: 480px) {
    .fields {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
