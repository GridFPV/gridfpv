<script lang="ts">
  /**
   * ServerSignInServices — the sign-in services of a server, for an owner of it
   * (docs/primitives.md §13.8): Discord and Google, each set up with the two values of an
   * app registered with it.
   *
   * The app's secret is typed here and never shown again: the server only says whether it
   * has one. A service in the server's own settings file still works. What is saved here
   * is used in its place, from the next sign-in on.
   */
  import { Banner, Button, Card, Dialog, InfoTip, Input } from '@gridfpv/components';
  import type { ServerSignInService } from '@gridfpv/types';
  import type { PeopleApi } from '../lib/serverPeople.js';

  let {
    api,
    onchange
  }: {
    api: Pick<PeopleApi, 'signInServices' | 'setSignInService' | 'removeSignInService'>;
    /** Told when what people can sign in with may have changed. */
    onchange?: () => void;
  } = $props();

  let services = $state<ServerSignInService[] | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  /** The id of the service being set up, while its box is open. */
  let editing = $state<string | undefined>(undefined);
  /** The owner is being asked whether to take that service off. */
  let removing = $state(false);
  let clientId = $state('');
  let secret = $state('');
  let error = $state<string | undefined>(undefined);
  let busy = $state(false);
  let copied = $state(false);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));

  const SOURCE: Record<ServerSignInService['source'], string> = {
    page: 'On',
    env: 'On, from the settings file',
    off: 'Off'
  };
  /** Where an app is registered with each provider. */
  const WHERE: Record<string, { site: string; href: string }> = {
    discord: {
      site: 'Discord Developer Portal',
      href: 'https://discord.com/developers/applications'
    },
    google: {
      site: 'Google Cloud Console',
      href: 'https://console.cloud.google.com/apis/credentials'
    }
  };

  $effect(() => {
    api
      .signInServices()
      .then((found) => {
        services = found;
        loadError = undefined;
      })
      .catch((err) => (loadError = say(err)));
  });

  const open = $derived(services?.find((service) => service.id === editing));
  /** A secret is needed unless one set on the page is kept. */
  const keeps = $derived(open?.source === 'page');
  const ready = $derived(clientId.trim() !== '' && (keeps || secret.trim() !== ''));

  function edit(service: ServerSignInService) {
    editing = service.id;
    removing = false;
    // The id from the settings file is shown, to be changed or kept.
    clientId = service.client_id ?? '';
    secret = '';
    error = undefined;
    copied = false;
  }

  function close() {
    editing = undefined;
    removing = false;
    error = undefined;
  }

  async function save(e: Event) {
    e.preventDefault();
    if (busy || !ready || !open) return;
    busy = true;
    error = undefined;
    try {
      services = await api.setSignInService(open.id, {
        client_id: clientId.trim(),
        ...(secret.trim() === '' ? {} : { client_secret: secret.trim() })
      });
      close();
      onchange?.();
    } catch (err) {
      error = say(err);
    } finally {
      busy = false;
    }
  }

  async function remove() {
    if (busy || !open) return;
    busy = true;
    error = undefined;
    try {
      services = await api.removeSignInService(open.id);
      close();
      onchange?.();
    } catch (err) {
      error = say(err);
    } finally {
      busy = false;
    }
  }

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied = true;
    } catch {
      // The address is on the page to be copied by hand.
    }
  }
</script>

<Card elevation="sm">
  <div class="block" data-testid="sign-in-services">
    <div class="row">
      <h2>Sign-in services</h2>
      <InfoTip
        text="People sign in with an account they already have. You register a free app with Discord or Google once, and give its two values here."
        label="About sign-in services"
      />
    </div>
    {#if loadError}<Banner tone="danger">{loadError}</Banner>{/if}
    {#if services === undefined && !loadError}
      <p class="state" role="status">Loading…</p>
    {:else if services}
      {#if services.some((service) => service.redirect === undefined)}
        <Banner tone="warn" title="This server does not know its own address.">
          <span data-testid="services-no-address"
            >Nobody can sign in with a service until GRIDFPV_SERVER_PUBLIC_URL is set.</span
          >
        </Banner>
      {/if}
      <ul aria-label="Sign-in services">
        {#each services as service (service.id)}
          <li data-testid="sign-in-service">
            <span class="name">{service.name}</span>
            <span class="state" data-testid="service-state">{SOURCE[service.source]}</span>
            {#if service.editable}
              <Button
                size="sm"
                variant={service.source === 'off' ? 'secondary' : 'ghost'}
                aria-label={service.source === 'off'
                  ? `Set up ${service.name}`
                  : `Change ${service.name}`}
                onclick={() => edit(service)}
                >{service.source === 'off' ? 'Set up' : 'Change'}</Button
              >
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  </div>
</Card>

{#if open && removing}
  <Dialog open={true} title="Remove {open.name}?" onclose={close}>
    <p>Nobody can sign in with {open.name} until it is set up again.</p>
    <p class="state">
      A {open.name} app in the server's settings file is used again, if it has one.
    </p>
    {#if error}<Banner tone="danger">{error}</Banner>{/if}
    {#snippet footer()}
      <Button variant="ghost" onclick={() => ((removing = false), (error = undefined))}
        >Cancel</Button
      >
      <Button variant="danger" disabled={busy} onclick={remove}>Remove</Button>
    {/snippet}
  </Dialog>
{:else if open}
  {@const where = WHERE[open.id]}
  <Dialog open={true} title="Sign in with {open.name}" onclose={close}>
    <form
      id="service-form"
      class="fields"
      onsubmit={save}
      oninput={() => (error = undefined)}
      aria-label="{open.name} app"
    >
      {#if where}
        <p class="wide state">
          Make an app at the
          <a href={where.href} target="_blank" rel="noreferrer noopener">{where.site}</a>, then copy
          its two values here.
        </p>
      {/if}
      {#if open.redirect}
        <span class="labelled">
          <span class="label" id="service-redirect-label">Redirect</span>
          <InfoTip
            text="Give this address to {open.name} as the app's redirect. It must match exactly."
            label="About the redirect"
          />
        </span>
        <span class="redirect">
          <code aria-labelledby="service-redirect-label" data-testid="service-redirect"
            >{open.redirect}</code
          >
          <Button size="sm" variant="ghost" onclick={() => copy(open.redirect ?? '')}
            >{copied ? 'Copied' : 'Copy'}</Button
          >
        </span>
      {/if}
      <label for="service-client-id">Client ID</label>
      <Input
        id="service-client-id"
        maxlength={300}
        autocomplete="off"
        autocapitalize="none"
        spellcheck="false"
        bind:value={clientId}
        disabled={busy}
      />
      <span class="labelled">
        <label for="service-secret">Client secret</label>
        <InfoTip
          text="Kept on the server and never shown again.{keeps
            ? ' Leave it empty to keep the one it has.'
            : ''}"
          label="About the secret"
        />
      </span>
      <Input
        id="service-secret"
        type="password"
        maxlength={300}
        autocomplete="new-password"
        placeholder={keeps ? 'Kept' : ''}
        bind:value={secret}
        disabled={busy}
      />
      {#if error}
        <p class="error wide" role="alert" data-testid="service-error">{error}</p>
      {/if}
    </form>
    {#snippet footer()}
      {#if open.source === 'page'}
        <Button
          variant="ghost"
          disabled={busy}
          onclick={() => ((error = undefined), (removing = true))}>Remove…</Button
        >
      {/if}
      <Button variant="ghost" onclick={close}>Cancel</Button>
      <Button variant="primary" type="submit" form="service-form" disabled={busy || !ready}
        >Save</Button
      >
    {/snippet}
  </Dialog>
{/if}

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
    min-height: 2rem;
  }
  .name {
    flex: 1;
  }
  .fields {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    align-items: center;
    gap: var(--gf-space-3);
    min-width: min(30rem, 80vw);
  }
  .fields label,
  .label {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .labelled {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
  .redirect {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    min-width: 0;
  }
  code {
    min-width: 0;
    padding: var(--gf-space-2) var(--gf-space-3);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    font-size: var(--gf-font-size-sm);
    overflow-wrap: anywhere;
  }
  .wide {
    grid-column: 1 / -1;
  }
  .state {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .error {
    color: var(--gf-danger);
    font-size: var(--gf-font-size-sm);
  }
  @media (max-width: 480px) {
    .fields {
      grid-template-columns: minmax(0, 1fr);
      min-width: 0;
    }
  }
</style>
