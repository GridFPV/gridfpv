<script lang="ts">
  /**
   * DataPage — move everything Grid keeps to another computer.
   *
   * **Save a copy** has the Director write one zip of the pilots, classes, formats, seat
   * profiles, timers and every event. The page says where the file is on the Director's
   * computer and offers it as a download, for when the console is open somewhere else.
   *
   * **Bring in a copy** hands the Director such a file. It replaces what this computer has,
   * so it asks first. The Director only checks the file and keeps it: the swap happens when
   * Grid is next started, and what was here goes into a backups folder. Until then the page
   * says a copy is waiting and lets the director call it off.
   */
  import { Banner, Button, Card, Dialog, toast } from '@gridfpv/components';
  import type { DataCopy, DataExport, DataInfo } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import type { Session } from '../lib/session.svelte.js';

  let {
    session
  }: {
    session: Pick<Session, 'getData' | 'exportData' | 'exportUrl' | 'importData' | 'cancelImport'>;
  } = $props();

  let info = $state<DataInfo | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);
  let made = $state<DataExport | undefined>(undefined);
  let busy = $state(false);
  let error = $state<string | undefined>(undefined);
  /** The file picked to bring in, while the page asks whether to go ahead. */
  let picked = $state<File | undefined>(undefined);
  let picker = $state<HTMLInputElement | undefined>(undefined);

  const say = (err: unknown): string => (err instanceof Error ? err.message : String(err));
  const events = (n: number): string => `${n} ${n === 1 ? 'event' : 'events'}`;
  const when = (copy: DataCopy): string => new Date(copy.made_at / 1000).toLocaleString();

  async function load() {
    try {
      info = await session.getData();
      loadError = undefined;
    } catch (err) {
      loadError = say(err);
    }
  }
  $effect(() => {
    void load();
  });

  async function act(what: () => Promise<void>) {
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

  const save = () =>
    act(async () => {
      const done = await session.exportData();
      if (done) made = done;
    });

  function choose(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    picked = input.files?.[0];
    // So picking the same file again still counts as a change.
    input.value = '';
  }

  const bringIn = () =>
    act(async () => {
      const file = picked;
      picked = undefined;
      if (!file) return;
      const copy = await session.importData(file);
      if (!copy) return;
      await load();
      toast.success(`The copy is ready. Close Grid and open it again to finish.`);
    });

  const callOff = () =>
    act(async () => {
      if (await session.cancelImport()) await load();
    });
</script>

<Page
  title="Data"
  help="data"
  lede="Move your pilots, classes, formats, timers and events to another computer."
>
  {#if loadError}
    <Banner tone="danger" title="Grid could not say where its data is.">{loadError}</Banner>
  {/if}
  {#if error}
    <Banner tone="danger">{error}</Banner>
  {/if}

  {#if info}
    {#if !info.folder}
      <Banner tone="warn" title="Grid is not keeping its data in a folder.">
        Nothing here outlasts a restart, so there is nothing to copy.
      </Banner>
    {:else}
      {#if info.waiting}
        <Banner tone="warn" title="A copy is waiting. Close Grid and open it again to finish.">
          <p>
            It has {events(info.waiting.events)} and was made {when(info.waiting)}. It replaces what
            is on this computer. What is here now goes into the <code>backups</code> folder.
          </p>
          <Button disabled={busy} onclick={callOff}>Do not bring it in</Button>
        </Banner>
      {/if}

      <Card title="Save a copy" elevation="sm">
        <p>
          One file with everything: pilots, classes, formats, seat profiles, timers and every event.
          Video setup and recordings are not in it.
        </p>
        <div class="row">
          <Button variant="primary" disabled={busy} onclick={save}>Save a copy</Button>
        </div>
        {#if made}
          <Banner tone="success" title={`Saved, with ${events(made.copy.events)}.`}>
            <p>On the computer running Grid it is at <code>{made.file}</code></p>
            <a href={session.exportUrl(made.name)} download={made.name}>Download it here</a>
          </Banner>
        {/if}
      </Card>

      <Card title="Bring in a copy" elevation="sm">
        <p>
          Pick a file saved from this page on another computer. It replaces what this computer has.
          Nothing is deleted: what is here now is moved to the <code>backups</code> folder.
        </p>
        <div class="row">
          <input
            bind:this={picker}
            class="picker"
            type="file"
            accept=".zip,application/zip"
            aria-label="Copy to bring in"
            onchange={choose}
          />
          <Button disabled={busy} onclick={() => picker?.click()}>Choose a file</Button>
        </div>
      </Card>
    {/if}
  {/if}
</Page>

{#if picked}
  <Dialog open={true} title="Bring in this copy?" onclose={() => (picked = undefined)}>
    <div class="body">
      <p><code>{picked.name}</code></p>
      <p>
        It replaces the pilots, classes, formats, timers and events on this computer. The swap
        happens when Grid is next started. What is here now is kept in the
        <code>backups</code> folder.
      </p>
    </div>
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (picked = undefined)}>Cancel</Button>
      <Button variant="primary" onclick={bringIn}>Bring it in</Button>
    {/snippet}
  </Dialog>
{/if}

<style>
  p {
    margin: 0 0 var(--space-3, 0.75rem);
    max-width: 65ch;
  }
  code {
    overflow-wrap: anywhere;
  }
  .row {
    display: flex;
    gap: var(--space-3, 0.75rem);
    align-items: center;
    margin-bottom: var(--space-3, 0.75rem);
  }
  .picker {
    position: absolute;
    width: 1px;
    height: 1px;
    opacity: 0;
    pointer-events: none;
  }
  .body {
    display: grid;
    gap: var(--space-2, 0.5rem);
  }
</style>
