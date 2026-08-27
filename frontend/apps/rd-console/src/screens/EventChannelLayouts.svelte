<script lang="ts">
  /**
   * EventChannelLayouts — the in-event **channel layout** editor (#117 S2).
   *
   * A **layout** is one complete tuning of the event's timer: `node → channel`, one channel per
   * enabled node, drawn from the channels the RD ticked for that timer on the Timers page. Three
   * scopes answer three different questions, and this screen is the middle one:
   *
   * | scope            | question                        | where                        |
   * | ---------------- | ------------------------------- | ---------------------------- |
   * | Global (a timer) | what may this timer *ever* use? | the Timers page's checkboxes |
   * | **Event**        | **what goes on which node?**    | **here**                     |
   * | Heat             | which layout does it fly?        | S3, not built                |
   *
   * ## Why this screen exists at all
   *
   * The event workspace embeds the *same* `TimerManager` as the global Timers page, so ticking a
   * channel "in the event" edits `Timer.available_channels` — **the global timer record**. That is
   * the bug underneath this slice. Layouts are event state: they live on the event's meta beside its
   * timers / roster / classes, and nothing here writes to a timer. Global is the seed (a new layout
   * is *seeded* from the allowed set, in the RD's own preference order); the event owns what it
   * runs. Deliberately the same layering as #411's base profile → event tune.
   *
   * ## The RD picks the strategy, not the system
   *
   * A bracket is **one layout for the whole tournament** — n channels for n pilots per heat, and they
   * never move. A GQ-style qualifier is **many layouts**, so each pilot can stay on their own
   * channel. Both fall out of one mechanism with no special case, which is why nothing here nudges
   * toward either.
   *
   * ## One hard rule, one warning
   *
   * Inside a layout, two nodes on the same channel is an **error** — a node cannot share a frequency
   * with its neighbour — and so is leaving an enabled node untuned (a layout is a *complete* tuning).
   * Both block Save, here and at the Director.
   *
   * Channel reuse **between** layouts is a **warning and never a block**. It only matters for the
   * keep-pilots-on-one-channel strategy; an RD running a bracket off one layout does not care. The
   * Director computes the overlaps and hands them back on a **200** — this screen renders them as
   * notices under the list.
   *
   * ## The IMD reading (#117 S4)
   *
   * A layout is defined once and flown all event, so this is the one moment an RD can still act on
   * how cleanly its channels fly together — the RD asked for exactly that: *"live IMD info, support
   * as I am picking channels for a layout"*. Each saved layout carries its rating, and the editor
   * reads the draft live as channels are ticked.
   *
   * The number comes from the **Director**, which owns the only implementation of IMDTabler in the
   * system (#430): its whole value is being the same number the RD reads off RotorHazard for the
   * same channels. It is **information and never a refusal** — a poor rating saves like any other,
   * because a Raceband-only timer genuinely cannot beat 0 at five pilots — and it carries **no
   * verdict word and no clean/marginal/poor band**, because the achievable ceiling collapses with
   * pilot count and a flat band would call every six-pilot layout dirty.
   *
   * Every value a person reads goes through the shared resolvers: `Node 3 · Raceband R7`, never an
   * index, a bare MHz, or a layout id (`channels.ts`'s `nodeSeatLabel` / `channelLabel`,
   * `timerNodes.ts`'s `nodeLabel`, and `channelLayouts.ts`'s `layoutName`).
   */
  import { Banner, Button, Card, Field, Input, Select, toast } from '@gridfpv/components';
  import type {
    ChannelCatalogEntry,
    ChannelLayout,
    ImdReading,
    LayoutId,
    LayoutOverlap,
    LayoutRating,
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
    draftChannelSet,
    draftNodes,
    duplicateNodes,
    imdMessage,
    layerNodes,
    layerSummary,
    layoutRating,
    overlapMessage,
    unconfiguredTimerMessage
  } from '../lib/channelLayouts.js';

  let {
    session,
    timer
  }: {
    session: Session;
    /**
     * The event's **effective primary** timer — the one a layout tunes. #112's redundant timers sit
     * at *one gate*, so an alternate taking over mid-event has to be listening on the same channels:
     * one layout per event, validated against the primary, is the honest model rather than one layout
     * per timer. Passed in (rather than read off the session) because the embedding screen already
     * holds the registry list and the effective-primary rule.
     */
    timer: Timer | undefined;
  } = $props();

  // ── Reads ────────────────────────────────────────────────────────────────
  let layouts = $state<ChannelLayout[]>([]);
  let overlaps = $state<LayoutOverlap[]>([]);
  /** Each layout's IMD reading (#117 S4) — advisory, computed by the Director, never a blocker. */
  let ratings = $state<LayoutRating[]>([]);
  let catalog = $state<ChannelCatalogEntry[]>([]);
  let nodeView = $state<TimerNodes | undefined>(undefined);
  let loadError = $state<string | undefined>(undefined);

  // ── The draft being edited ───────────────────────────────────────────────
  // `open` is the editor's visibility; `draftId` distinguishes editing an existing layout from
  // adding a new one (which is what makes the seed path reachable — see `save`).
  let open = $state(false);
  let draftId = $state<LayoutId | undefined>(undefined);
  let draftName = $state('');
  let draftChannels = $state<Map<number, number>>(new Map());
  let saving = $state(false);

  const draft = $derived({ id: draftId, name: draftName, channels: draftChannels });

  // Re-read the layouts whenever the event's meta is re-homed — which includes this screen's own
  // writes (the session re-homes `currentEvent` so its cached `channel_layouts` never goes stale).
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

  // The node set a layout tunes comes from the Director's own node view (#412) — never re-derived
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
      const view = await session.listChannelLayouts();
      layouts = view.layouts;
      overlaps = view.overlaps ?? [];
      ratings = view.ratings ?? [];
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
   * A brand-new layout with nothing picked yet — the **seed** path. The create request then carries
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

  // ── The live IMD reading (#117 S4) ────────────────────────────────────────
  //
  // The RD asked for this exactly: *"it would be nice to have some live IMD info, support as I am
  // picking channels for a layout"*. A layout is defined once and flown all event, so this is the
  // one moment the information can still change the outcome.
  //
  // The number comes from the **Director**, which owns the only implementation of IMDTabler in the
  // system. That is #430 taken seriously: the whole value of the rating is that it is the number
  // the RD already reads off RotorHazard for the same channels, and a second port of the algorithm
  // in the console is precisely how that stops being true. The reading is a pure function of the
  // channel set, so it caches perfectly — ticking back and forth between two channels re-reads a
  // set already answered, and never asks twice.

  /** Readings already answered, keyed by the channel set. Pure over its query, so it never goes stale. */
  const imdCache = new Map<string, ImdReading>();
  let draftImd = $state<ImdReading | undefined>(undefined);
  /** A read is in flight for a set we have no answer for yet — the strip says so rather than lying. */
  let imdPending = $state(false);

  /** The draft's channels as a set — ascending and de-duplicated, so the cache key is stable. */
  const draftSet = $derived(open ? draftChannelSet(draft) : []);
  /**
   * Whether there is anything to rate. Under two channels there are no mixing products at all and
   * the Director would answer a flattering `100` — which an RD would read as "this layout is
   * perfect" rather than "you have picked one channel". Two nodes briefly on the same channel is a
   * draft state the blocker already speaks to; rating the collapsed set would answer about a
   * different layout than the one on screen.
   */
  const rateable = $derived(open && clashing.size === 0 && draftSet.length >= 2);

  $effect(() => {
    if (!rateable) {
      draftImd = undefined;
      imdPending = false;
      return;
    }
    const key = draftSet.join(',');
    const known = imdCache.get(key);
    if (known) {
      draftImd = known;
      imdPending = false;
      return;
    }
    // Debounced: an RD working down a column of dropdowns changes the set several times a second,
    // and the intermediate sets are not sets they are asking about.
    imdPending = true;
    const wanted = [...draftSet];
    const timer = setTimeout(() => {
      session
        .rateChannels(wanted)
        .then((reading) => {
          imdCache.set(key, reading);
          // Only answer if this is still the set on screen — an out-of-order response must never
          // put one set's rating against another's channels.
          if (draftSet.join(',') !== key) return;
          draftImd = reading;
          imdPending = false;
        })
        .catch(() => {
          // Information, never a blocker: a failed read shows nothing and the RD carries on saving.
          if (draftSet.join(',') !== key) return;
          draftImd = undefined;
          imdPending = false;
        });
    }, 120);
    return () => clearTimeout(timer);
  });

  // ── Editing ──────────────────────────────────────────────────────────────

  function startAdd() {
    draftId = undefined;
    draftName = `Layout ${String.fromCharCode(65 + layouts.length)}`;
    draftChannels = new Map();
    open = true;
  }

  function startEdit(layout: ChannelLayout) {
    draftId = layout.id;
    draftName = layout.name;
    draftChannels = new Map(layout.nodes.map((n) => [n.node, n.channel]));
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
        ? await session.updateChannelLayout(draftId, { name: draftName.trim(), nodes })
        : await session.createChannelLayout({
            name: draftName.trim(),
            // Omitted on the seed path — see `seeding`.
            ...(nodes.length > 0 ? { nodes } : {})
          });
      if (!view) {
        toast.info('A control token is required to edit this event’s channel layouts.');
        return;
      }
      layouts = view.layouts;
      overlaps = view.overlaps ?? [];
      ratings = view.ratings ?? [];
      open = false;
    } catch (e) {
      // The Director's refusal is already written for the RD — it names the node, the channel and
      // the timer by their friendly names. Surfaced verbatim rather than re-worded.
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      saving = false;
    }
  }

  async function remove(layout: ChannelLayout) {
    try {
      const view = await session.deleteChannelLayout(layout.id);
      if (!view) {
        toast.info('A control token is required to edit this event’s channel layouts.');
        return;
      }
      layouts = view.layouts;
      overlaps = view.overlaps ?? [];
      ratings = view.ratings ?? [];
      if (draftId === layout.id) open = false;
      toast.success(`Removed ${layout.name}.`);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    }
  }
</script>

<Card
  title="Channel layouts"
  subtitle="What goes on which node in this event. A layout tunes every node; edits here stay in this event."
>
  {#snippet actions()}
    <Button variant="secondary" size="sm" disabled={!!unconfigured || open} onclick={startAdd}>
      + Add layout
    </Button>
  {/snippet}

  {#if loadError}
    <Banner tone="danger" title="Couldn’t read this event’s channel layouts.">{loadError}</Banner>
  {/if}

  {#if unconfigured}
    <!-- The empty-allowed-set trap, headed off: an empty set is "the RD has not configured this
         timer", never "this timer has no channels". Say which page fixes it. -->
    <Banner tone="warn">{unconfigured}</Banner>
  {:else}
    {#if layouts.length === 0 && !open}
      <p class="empty">
        No layouts yet. Add one and it starts from the channels {timer?.name} is allowed to use — then
        change any node you like. A bracket usually needs one layout for the whole tournament; qualifiers
        that keep each pilot on their own channel need several.
      </p>
    {/if}

    {#if layouts.length > 0}
      <ul class="layouts" aria-label="Channel layouts">
        {#each layouts as layout (layout.id)}
          <li class="layout-row" class:editing={open && draftId === layout.id}>
            <div class="layout-body">
              <span class="layout-name">{layout.name}</span>
              <span class="layout-tuning">{layerSummary(layout, catalog)}</span>
              {#if layoutRating(ratings, layout.id)}
                <!-- The rating and the worst offender behind it (#117 S4) — plain information,
                     with no verdict word and no clean/marginal/poor band. See `imdMessage`. -->
                <span class="layout-imd"
                  >{imdMessage(layoutRating(ratings, layout.id)!, catalog)}</span
                >
              {/if}
            </div>
            <div class="layout-actions">
              <Button
                variant="secondary"
                size="sm"
                disabled={open}
                onclick={() => startEdit(layout)}
              >
                Edit
              </Button>
              <ConfirmButton variant="danger" onconfirm={() => remove(layout)}>Remove</ConfirmButton
              >
            </div>
          </li>
        {/each}
      </ul>
    {/if}

    {#each overlaps as overlap (`${overlap.layout}|${overlap.other}`)}
      <!-- A NOTICE, never a block: reuse only matters for the keep-pilots-on-one-channel
           strategy, so it is informative and ignorable by design. -->
      <Banner tone="info">{overlapMessage(overlap, layouts, catalog)}</Banner>
    {/each}

    {#if open}
      <form
        class="editor"
        aria-label={draftId ? 'Edit channel layout' : 'New channel layout'}
        onsubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <Field label="Layout name" required>
          <Input bind:value={draftName} aria-label="Layout name" placeholder="Bracket A" />
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

        <!-- Live IMD, as the RD picks (#117 S4). Deliberately toneless: it is never a verdict, it
             never blocks Save, and it carries no threshold — the achievable ceiling collapses with
             pilot count, so a flat "clean/poor" band would call every six-pilot layout dirty. -->
        <div class="imd" aria-live="polite" aria-label="IMD reading">
          {#if seeding}
            <!-- Nothing is picked, so there is nothing to rate: the Director chooses the seed and
                 its reading comes back with the saved layout. Predicting the seed here would be a
                 second implementation of a rule the Director owns. -->
            <span class="imd-hint"
              >This layout has not picked any channels yet — its IMD reading appears once it is
              saved, or as soon as you set one here.</span
            >
          {:else if !open || draftSet.length === 0}
            <span class="imd-hint">Pick channels and their IMD reading appears here.</span>
          {:else if clashing.size > 0}
            <span class="imd-hint"
              >Two nodes are on one channel — settle that and the IMD reading returns.</span
            >
          {:else if draftSet.length < 2}
            <span class="imd-hint"
              >One channel cannot interfere with anything. Pick a second to see how they fly
              together.</span
            >
          {:else if draftImd}
            <span class="imd-line" class:stale={imdPending}>{imdMessage(draftImd, catalog)}</span>
          {:else if imdPending}
            <span class="imd-hint">Reading these channels…</span>
          {:else}
            <span class="imd-hint"
              >Couldn’t read the IMD for these channels. It does not affect saving.</span
            >
          {/if}
          <span class="imd-note">
            Higher is cleaner and 100 is the ceiling — the same number RotorHazard shows for these
            channels. What is achievable falls as you use more nodes, so compare layouts against
            each other rather than against 100.
          </span>
        </div>

        {#if blocker}
          <Banner tone="warn">{blocker}</Banner>
        {:else if seeding}
          <p class="seed-note">
            Leave the channels unset and this layout starts from the channels {timer?.name} is allowed
            to use, one per node.
          </p>
        {/if}

        <div class="editor-actions">
          <Button variant="secondary" type="button" onclick={cancel}>Cancel</Button>
          <Button variant="primary" type="submit" disabled={saving || !!blocker}>
            {draftId ? 'Save layout' : 'Add layout'}
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
  .layouts {
    list-style: none;
    margin: 0 0 var(--gf-space-3);
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .layout-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-3);
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface-alt);
  }
  .layout-row.editing {
    border-color: var(--gf-accent);
  }
  .layout-body {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .layout-name {
    font-weight: var(--gf-font-weight-semibold);
  }
  /* The tuning is real data, not chrome: it is what the RD reads off to set a pilot's VTX. */
  .layout-tuning {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  /* Deliberately the same muted treatment whatever the rating says. Colouring it would BE the
     verdict the RD asked us not to give. */
  .layout-imd {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .imd {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: var(--gf-font-size-sm);
  }
  .imd-line {
    color: var(--gf-text);
  }
  /* An answer for a set the RD has already moved on from: shown, but visibly not settled. */
  .imd-line.stale {
    opacity: 0.5;
  }
  .imd-hint,
  .imd-note {
    color: var(--gf-text-muted);
  }
  .imd-note {
    font-size: var(--gf-font-size-xs, var(--gf-font-size-sm));
  }
  .layout-actions {
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
