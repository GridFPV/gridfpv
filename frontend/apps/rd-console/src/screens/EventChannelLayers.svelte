<script lang="ts">
  /**
   * EventChannelLayers — the in-event **channel layer** editor (#117 S2).
   *
   * A **layer** is one complete tuning of the event's timer: `node → channel`, one channel per
   * enabled node, drawn from the channels the RD ticked for that timer on the Timers page. Three
   * scopes answer three different questions, and this screen is the middle one:
   *
   * | scope            | question                        | where                        |
   * | ---------------- | ------------------------------- | ---------------------------- |
   * | Global (a timer) | what may this timer *ever* use? | the Timers page's checkboxes |
   * | **Event**        | **what goes on which node?**    | **here**                     |
   * | Heat             | which layer does it fly?        | S3, not built                |
   *
   * ## Why this screen exists at all
   *
   * The event workspace embeds the *same* `TimerManager` as the global Timers page, so ticking a
   * channel "in the event" edits `Timer.available_channels` — **the global timer record**. That is
   * the bug underneath this slice. Layers are event state: they live on the event's meta beside its
   * timers / roster / classes, and nothing here writes to a timer. Global is the seed (a new layer
   * is *seeded* from the allowed set, in the RD's own preference order); the event owns what it
   * runs. Deliberately the same layering as #411's base profile → event tune.
   *
   * ## The RD picks the strategy, not the system
   *
   * A bracket is **one layer for the whole tournament** — n channels for n pilots per heat, and they
   * never move. A GQ-style qualifier is **many layers**, so each pilot can stay on their own
   * channel. Both fall out of one mechanism with no special case, which is why nothing here nudges
   * toward either.
   *
   * ## One hard rule, one warning
   *
   * Inside a layer, two nodes on the same channel is an **error** — a node cannot share a frequency
   * with its neighbour — and so is leaving an enabled node untuned (a layer is a *complete* tuning).
   * Both block Save, here and at the Director.
   *
   * Channel reuse **between** layers is a **warning and never a block**. It only matters for the
   * keep-pilots-on-one-channel strategy; an RD running a bracket off one layer does not care. The
   * Director computes the overlaps and hands them back on a **200** — this screen renders them as
   * notices under the list.
   *
   * Every value a person reads goes through the shared resolvers: `Node 3 · Raceband R7`, never an
   * index, a bare MHz, or a layer id (`channels.ts`'s `nodeSeatLabel` / `channelLabel`,
   * `timerNodes.ts`'s `nodeLabel`, and `channelLayers.ts`'s `layerName`).
   */
  import { Banner, Button, Card, Field, Input, Select, toast } from '@gridfpv/components';
  import type {
    ChannelCatalogEntry,
    ChannelLayer,
    LayerId,
    LayerOverlap,
    Timer,
    TimerNodes
  } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import ConfirmButton from '../lib/ConfirmButton.svelte';
  import { channelLabel } from '../lib/channels.js';
  import { nodeLabel } from '../lib/timerNodes.js';
  import {
    allowedChannels,
    draftBlocker,
    draftNodes,
    duplicateNodes,
    layerNodes,
    layerSummary,
    overlapMessage,
    unconfiguredTimerMessage
  } from '../lib/channelLayers.js';

  let {
    session,
    timer
  }: {
    session: Session;
    /**
     * The event's **effective primary** timer — the one a layer tunes. #112's redundant timers sit
     * at *one gate*, so an alternate taking over mid-event has to be listening on the same channels:
     * one layer per event, validated against the primary, is the honest model rather than one layer
     * per timer. Passed in (rather than read off the session) because the embedding screen already
     * holds the registry list and the effective-primary rule.
     */
    timer: Timer | undefined;
  } = $props();

  // ── Reads ────────────────────────────────────────────────────────────────
  let layers = $state<ChannelLayer[]>([]);
  let overlaps = $state<LayerOverlap[]>([]);
  let catalog = $state<ChannelCatalogEntry[]>([]);
  let nodeView = $state<TimerNodes | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);

  // ── The draft being edited ───────────────────────────────────────────────
  // `open` is the editor's visibility; `draftId` distinguishes editing an existing layer from
  // adding a new one (which is what makes the seed path reachable — see `save`).
  let open = $state(false);
  let draftId = $state<LayerId | undefined>(undefined);
  let draftName = $state('');
  let draftChannels = $state<Map<number, number>>(new Map());
  let saving = $state(false);

  const draft = $derived({ id: draftId, name: draftName, channels: draftChannels });

  // Re-read the layers whenever the event's meta is re-homed — which includes this screen's own
  // writes (the session re-homes `currentEvent` so its cached `channel_layers` never goes stale).
  // The write already applied its response locally, so this is a confirmation rather than the way
  // the list arrives: the RD sees the change immediately and the read settles it.
  $effect(() => {
    void loadLayers();
  });

  // The catalog is an open read, loaded once; a failed load degrades to labels falling back to MHz
  // rather than an empty screen (TimerManager does the same).
  $effect(() => {
    session
      .listChannels()
      .then((c) => (catalog = c))
      .catch(() => (catalog = []));
  });

  // The node set a layer tunes comes from the Director's own node view (#412) — never re-derived
  // here, because an off-by-one in that boundary puts a pilot on a dead gate.
  $effect(() => {
    const id = timer?.id;
    if (!id) {
      nodeView = undefined;
      return;
    }
    session
      .timerNodes(id)
      .then((view) => (nodeView = view))
      .catch(() => (nodeView = undefined));
  });

  async function loadLayers() {
    try {
      const view = await session.listChannelLayers();
      layers = view.layers;
      overlaps = view.overlaps ?? [];
      loadError = undefined;
    } catch (e) {
      loadError = e instanceof Error ? e.message : String(e);
    }
  }

  // ── Derived readings ─────────────────────────────────────────────────────
  const unconfigured = $derived(unconfiguredTimerMessage(timer));
  /** The channels a node's dropdown offers: the timer's **allowed** set, in the RD's own order. */
  const offered = $derived(allowedChannels(timer));
  const tunableNodes = $derived(layerNodes(nodeView));
  const clashing = $derived(open ? duplicateNodes(draft) : new Set<number>());
  /**
   * A brand-new layer with nothing picked yet — the **seed** path. The create request then carries
   * no `nodes` and the Director lays the timer's allowed set onto the enabled nodes, which is how
   * "global is the default an event starts from" is actually spelled. So an untuned node is not a
   * blocker here; it is the whole point.
   */
  const seeding = $derived(open && !draftId && draftChannels.size === 0);
  const blocker = $derived(
    open ? draftBlocker(draft, seeding ? [] : tunableNodes, nodeView, catalog) : undefined
  );

  function label(node: number): string {
    return nodeView ? nodeLabel(nodeView, node) : `Node ${node + 1}`;
  }

  // ── Editing ──────────────────────────────────────────────────────────────

  function startAdd() {
    draftId = undefined;
    draftName = `Layer ${String.fromCharCode(65 + layers.length)}`;
    draftChannels = new Map();
    open = true;
  }

  function startEdit(layer: ChannelLayer) {
    draftId = layer.id;
    draftName = layer.name;
    draftChannels = new Map(layer.nodes.map((n) => [n.node, n.channel]));
    open = true;
  }

  function cancel() {
    open = false;
  }

  function setChannel(node: number, value: string) {
    const next = new Map(draftChannels);
    if (value === '') next.delete(node);
    else next.set(node, Number(value));
    draftChannels = next;
  }

  async function save() {
    if (!open || blocker || saving) return;
    saving = true;
    try {
      const nodes = draftNodes(draft);
      const view = draftId
        ? await session.updateChannelLayer(draftId, { name: draftName.trim(), nodes })
        : await session.createChannelLayer({
            name: draftName.trim(),
            // Omitted on the seed path — see `seeding`.
            ...(nodes.length > 0 ? { nodes } : {})
          });
      if (!view) {
        toast.info('A control token is required to edit this event’s channel layers.');
        return;
      }
      layers = view.layers;
      overlaps = view.overlaps ?? [];
      open = false;
    } catch (e) {
      // The Director's refusal is already written for the RD — it names the node, the channel and
      // the timer by their friendly names. Surfaced verbatim rather than re-worded.
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      saving = false;
    }
  }

  async function remove(layer: ChannelLayer) {
    try {
      const view = await session.deleteChannelLayer(layer.id);
      if (!view) {
        toast.info('A control token is required to edit this event’s channel layers.');
        return;
      }
      layers = view.layers;
      overlaps = view.overlaps ?? [];
      if (draftId === layer.id) open = false;
      toast.success(`Removed ${layer.name}.`);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    }
  }
</script>

<Card
  title="Channel layers"
  subtitle="What goes on which node in this event. A layer tunes every node; edits here stay in this event."
>
  {#snippet actions()}
    <Button variant="secondary" size="sm" disabled={!!unconfigured || open} onclick={startAdd}>
      + Add layer
    </Button>
  {/snippet}

  {#if loadError}
    <Banner tone="danger" title="Couldn’t read this event’s channel layers.">{loadError}</Banner>
  {/if}

  {#if unconfigured}
    <!-- The empty-allowed-set trap, headed off: an empty set is "the RD has not configured this
         timer", never "this timer has no channels". Say which page fixes it. -->
    <Banner tone="warn">{unconfigured}</Banner>
  {:else}
    {#if layers.length === 0 && !open}
      <p class="empty">
        No layers yet. Add one and it starts from the channels {timer?.name} is allowed to use — then
        change any node you like. A bracket usually needs one layer for the whole tournament; qualifiers
        that keep each pilot on their own channel need several.
      </p>
    {/if}

    {#if layers.length > 0}
      <ul class="layers" aria-label="Channel layers">
        {#each layers as layer (layer.id)}
          <li class="layer-row" class:editing={open && draftId === layer.id}>
            <div class="layer-body">
              <span class="layer-name">{layer.name}</span>
              <span class="layer-tuning">{layerSummary(layer, catalog)}</span>
            </div>
            <div class="layer-actions">
              <Button
                variant="secondary"
                size="sm"
                disabled={open}
                onclick={() => startEdit(layer)}
              >
                Edit
              </Button>
              <ConfirmButton variant="danger" onconfirm={() => remove(layer)}>Remove</ConfirmButton>
            </div>
          </li>
        {/each}
      </ul>
    {/if}

    {#each overlaps as overlap (`${overlap.layer}|${overlap.other}`)}
      <!-- A NOTICE, never a block: reuse only matters for the keep-pilots-on-one-channel
           strategy, so it is informative and ignorable by design. -->
      <Banner tone="info">{overlapMessage(overlap, layers, catalog)}</Banner>
    {/each}

    {#if open}
      <form
        class="editor"
        aria-label={draftId ? 'Edit channel layer' : 'New channel layer'}
        onsubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <Field label="Layer name" required>
          <Input bind:value={draftName} aria-label="Layer name" placeholder="Bracket A" />
        </Field>

        {#if tunableNodes.length === 0}
          <p class="empty">Reading {timer?.name}’s nodes…</p>
        {:else}
          <div class="nodes" role="group" aria-label="Node channels">
            {#each tunableNodes as node (node)}
              {@const chosen = draftChannels.get(node)}
              <Field
                label={label(node)}
                error={clashing.has(node) ? 'Shared with another node' : undefined}
              >
                <Select
                  value={chosen === undefined ? '' : String(chosen)}
                  aria-label={`Channel for ${label(node)}`}
                  onchange={(e: Event) =>
                    setChannel(node, (e.currentTarget as HTMLSelectElement).value)}
                >
                  <option value="">Not set</option>
                  {#each offered as mhz (mhz)}
                    <!-- The option VALUE is the raw MHz (a wire handle); the visible label is
                         always the band + channel name. -->
                    <option value={String(mhz)}>{channelLabel(mhz, catalog)}</option>
                  {/each}
                </Select>
              </Field>
            {/each}
          </div>
        {/if}

        {#if blocker}
          <Banner tone="warn">{blocker}</Banner>
        {:else if seeding}
          <p class="seed-note">
            Leave the channels unset and this layer starts from the channels {timer?.name} is allowed
            to use, one per node.
          </p>
        {/if}

        <div class="editor-actions">
          <Button variant="secondary" type="button" onclick={cancel}>Cancel</Button>
          <Button variant="primary" type="submit" disabled={saving || !!blocker}>
            {draftId ? 'Save layer' : 'Add layer'}
          </Button>
        </div>
      </form>
    {/if}
  {/if}
</Card>

<style>
  .empty,
  .seed-note {
    margin: 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .layers {
    list-style: none;
    margin: 0 0 var(--gf-space-3);
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .layer-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-3);
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface-alt);
  }
  .layer-row.editing {
    border-color: var(--gf-accent);
  }
  .layer-body {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .layer-name {
    font-weight: var(--gf-font-weight-semibold);
  }
  /* The tuning is real data, not chrome: it is what the RD reads off to set a pilot's VTX. */
  .layer-tuning {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .layer-actions {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-shrink: 0;
  }
  .editor {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    margin-top: var(--gf-space-3);
    padding-top: var(--gf-space-3);
    border-top: 1px solid var(--gf-border-subtle);
  }
  .nodes {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(11rem, 1fr));
    gap: var(--gf-space-3);
  }
  .editor-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--gf-space-2);
  }
</style>
