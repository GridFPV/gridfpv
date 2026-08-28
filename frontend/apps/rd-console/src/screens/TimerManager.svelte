<script lang="ts">
  /**
   * TimerManager — the shared timer **registry management** piece (issue #73, Slice 2b).
   *
   * The single home for timer CRUD: it lists every configured timer (name + a **kind badge**
   * Mock/RotorHazard + its `status` via `StatusPill`) and drives **add** / **edit** / **remove**
   * through one stacked Dialog. The built-in **Mock** is undeletable (its Remove is hidden) and a
   * stray 400 is surfaced gracefully. Ids are auto-generated server-side — never shown or asked for.
   *
   * It is used two ways, so the CRUD lives in one place:
   *  - the {@link TimersPage} embeds it **CRUD-only** (no event context to select for);
   *  - the in-event {@link EventTimers} screen embeds it and layers **per-event selection** on top,
   *    via the optional `rowLead` snippet (a checkbox per row) and `listHeader`/`listFooter`
   *    snippets (the selection count + Save).
   *
   * The list (and so the available-to-select set) refreshes after every create/edit/delete; the
   * latest `timers` are exposed back to the parent through the bindable `timers` prop and an
   * `onchange` callback so a selection owner can reconcile.
   */
  import {
    Badge,
    Button,
    Card,
    Dialog,
    Field,
    Input,
    Select,
    StatusPill,
    toast
  } from '@gridfpv/components';
  import type { Snippet } from 'svelte';
  import type {
    ChannelCapability,
    ChannelCatalogEntry,
    CreateTimerRequest,
    Timer,
    TimerKind,
    TimerNodes,
    UpdateTimerRequest
  } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import {
    DEFAULT_MOCK_LAPS,
    DEFAULT_MOCK_LAP_MS,
    connectActionLabel,
    connectionHint,
    isBuiltInMock,
    isConnectable,
    isManuallyHeld,
    isTimerConnected,
    kindLabel,
    kindSummary,
    kindTag,
    kindTone,
    type TimerKindTag
  } from '../lib/timers.js';
  import {
    bandSelection,
    capabilityTag,
    channelLabel,
    fixedAllowed,
    groupByBand,
    isPlausibleMhz,
    toggleBandSelection,
    type CapabilityTag,
    type ChannelBand
  } from '../lib/channels.js';
  import {
    DEFAULT_NODE_COUNT,
    clampExplanation,
    clampNodeCount,
    maxNodeCount,
    nodeCountHint,
    timerDrifts,
    timerNodeSummary,
    timerWidth
  } from '../lib/timerNodes.js';
  import PluginCallout from './PluginCallout.svelte';
  import TimerNodesDialog from './TimerNodesDialog.svelte';

  let {
    session,
    timers = $bindable([]),
    onchange,
    rowLead,
    rowNote,
    listHeader,
    listFooter,
    rowChecked,
    ontune
  }: {
    session: Session;
    /** The latest loaded registry, exposed so a selection owner can reconcile its working set. */
    timers?: Timer[];
    /** Called after every create/edit/delete (and the reload it triggers) with the fresh list. */
    onchange?: (timers: Timer[]) => void;
    /** Rendered at the **start** of each row (e.g. the per-event selection checkbox). */
    rowLead?: Snippet<[Timer]>;
    /**
     * Rendered **inside** the row, under its config lines — the per-embedder sentence about *this*
     * timer (e.g. #405's "why this one can't be selected for the event, and what to do about it").
     * Kept a snippet because the reason is the embedder's business: the standalone Timers page has
     * no event to refuse a selection for.
     */
    rowNote?: Snippet<[Timer]>;
    /** Rendered **above** the list (e.g. nothing today; reserved for selection chrome). */
    listHeader?: Snippet;
    /** Rendered **below** the list (e.g. the selection count + Save). */
    listFooter?: Snippet;
    /** Whether a row is currently selected — drives the row's "checked" highlight in select mode. */
    rowChecked?: (timer: Timer) => boolean;
    /**
     * Open the per-timer **Tune** page for a timer (#355). Both hosts offer it, and the *route* they
     * navigate to is the tuning **scope** (#411): the app-level Timers page opens the timer's own
     * baseline, the in-event screen opens that event's tune and returns into the event. Optional
     * still: an embedder with nowhere to navigate to (the setup wizard's embedded Timer step) simply
     * doesn't offer the action rather than offering one that goes nowhere. The row decides *which*
     * timers get it (see the button's `{#if}`).
     */
    ontune?: (timer: Timer) => void;
  } = $props();

  type LoadState =
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; timers: Timer[] };

  let loadState = $state<LoadState>({ kind: 'loading' });

  export async function load() {
    loadState = { kind: 'loading' };
    try {
      const list = await session.listTimers();
      loadState = { kind: 'ready', timers: list };
      timers = list;
    } catch (e) {
      loadState = { kind: 'error', message: e instanceof Error ? e.message : String(e) };
    }
  }

  // (Re)load on mount, so the list always reflects the current registry.
  $effect(() => {
    void load();
  });

  // ── The standard channel catalog (race redesign Slice 4b) ───────────────────
  // Loaded once from `GET /channels` (open read); the channel picker offers it grouped by band, and
  // the row summary resolves a timer's available raw-MHz set back to band+channel labels. A failed
  // load degrades to an empty catalog — custom raw-MHz entry still works, labels fall back to MHz.
  let catalog = $state<ChannelCatalogEntry[]>([]);
  const bands = $derived(groupByBand(catalog));
  $effect(() => {
    session
      .listChannels()
      .then((c) => (catalog = c))
      .catch(() => (catalog = []));
  });

  /**
   * A timer's **allowed** channels, summarised as band+channel labels (e.g. "Raceband R1, F4 …").
   *
   * An empty set is a **configuration gap, not a capability statement** (#117 S1). This line used to
   * read "No channels available", which is false on exactly the timers it fires for: every real
   * RotorHazard is `Flexible` — it can tune all 52 catalog channels — and lists none only because
   * nobody has ticked any yet. Worse, it read as *"nothing to do here"* when it is precisely the
   * thing to do: with an empty allowed set the server now refuses to seat a heat on this timer
   * (`NoChannelsAllowed`), and this row's picker is where the RD fixes it.
   */
  function channelSummary(timer: Timer): string {
    const list = timer.available_channels ?? [];
    if (list.length === 0) return 'No channels chosen — heats cannot be seated on this timer';
    const labels = list.map((mhz) => channelLabel(mhz, catalog));
    const shown = labels.slice(0, 4).join(', ');
    return labels.length > 4 ? `${shown} +${labels.length - 4} more` : shown;
  }

  /** Whether a timer has no allowed channels chosen — the summary above reads as a warning. */
  function channelsUnset(timer: Timer): boolean {
    return (timer.available_channels ?? []).length === 0;
  }

  /**
   * The rows to render: the loaded registry, but with each timer's **status** overlaid from the
   * session's live-polled {@link Session.timers} when available (#73, Slice 2b). The load fetches
   * the registry once (and on every CRUD); the in-event poll keeps the *status* fresh, so a timer
   * dropping off updates here too — the screen and the header read the same live value. Outside an
   * event (the picker's Timers modal) the poll is idle, so this is just the loaded list unchanged.
   */
  const displayTimers = $derived.by(() => {
    if (loadState.kind !== 'ready') return [];
    // Overlay the poll-fresh, in-memory fields (connection `status`, `plugin` presence AND the
    // node count the timer REPORTED) from the live-polled list; all three are driven by the live
    // connection and only refreshed there. `reported_nodes` (#412) is learned on connect, so
    // without it here a timer that has just come up would keep reading "no drift" until the
    // manager happened to reload — hiding exactly the disagreement the RD needs to see.
    const live = new Map(session.timers.map((t) => [t.id, t]));
    return loadState.timers.map((t) => {
      const l = live.get(t.id);
      return l ? { ...t, status: l.status, plugin: l.plugin, reported_nodes: l.reported_nodes } : t;
    });
  });

  /** Reload, then notify the parent so a selection owner can reconcile against the fresh set. */
  async function reload() {
    await load();
    if (loadState.kind === 'ready') onchange?.(loadState.timers);
  }

  // ── Manual connect / disconnect (issue #383) ────────────────────────────────
  // The RD's "is this timer even reachable?" control. It is deliberately usable **with no active
  // event**: that is the whole point of #383 — setting up at a venue, the RD needs to know whether
  // a URL is right and whether the plugin is installed *before* an event exists to select it for.
  // `POST /timers/{id}/connect` sets a server-side hold and the Director's reconciler dials it on
  // its next tick, publishing the same `status` + `plugin` the event-driven path does — so the
  // StatusPill and PluginCallout already on the row are the readout; nothing new is needed there.

  /**
   * How often this screen re-reads `GET /timers` while a manual hold is up (#383).
   *
   * The session's own timer poll only runs **inside an event** ({@link Session.selectEvent} starts
   * it), so outside one nothing would ever refresh `status` — the RD would press Connect and watch
   * a pill that never moved. This screen therefore polls for itself whenever a hold exists, which
   * is exactly the window where a moving status is the answer the RD is waiting for. Brisker than
   * the session poll because the RD is actively watching this row.
   */
  const HOLD_POLL_MS = 1500;

  /**
   * Whether any timer is currently manually held — the screen self-polls while one is.
   *
   * Reads the loaded list (kept fresh by the poll below and by {@link applyTimer}) **and** the
   * session's in-event list, so a hold another console placed still starts our poll.
   */
  const anyHeld = $derived(
    (loadState.kind === 'ready' && loadState.timers.some(isManuallyHeld)) ||
      session.timers.some(isManuallyHeld)
  );

  $effect(() => {
    if (!anyHeld) return;
    const handle = setInterval(() => void refreshQuietly(), HOLD_POLL_MS);
    return () => clearInterval(handle);
  });

  /**
   * Re-read the registry **without** flipping back to the loading state — a poll must not blank the
   * list the RD is reading. A failed poll keeps the last good list; the next tick retries.
   */
  async function refreshQuietly() {
    try {
      const list = await session.listTimers();
      if (loadState.kind === 'ready') loadState = { kind: 'ready', timers: list };
      timers = list;
    } catch {
      /* keep the last good list; the next tick retries */
    }
  }

  /** Fold one server-authoritative {@link Timer} back into the loaded list, in place. */
  function applyTimer(updated: Timer) {
    if (loadState.kind !== 'ready') return;
    const next = loadState.timers.map((t) => (t.id === updated.id ? updated : t));
    loadState = { kind: 'ready', timers: next };
    timers = next;
  }

  /** The timer whose connect/disconnect call is in flight (its row's button shows the spinner). */
  let connecting = $state<string | undefined>(undefined);

  /**
   * Toggle the manual connection hold on a timer. The response is the updated `Timer`, so the
   * button flips immediately — `status` follows a tick later once the reconciler acts, which the
   * hold poll above picks up.
   */
  async function toggleConnection(timer: Timer) {
    if (connecting) return;
    const held = isManuallyHeld(timer);
    connecting = timer.id;
    try {
      const updated = held
        ? await session.disconnectTimer(timer.id)
        : await session.connectTimer(timer.id);
      if (updated === undefined) {
        // The RD cancelled the token prompt on a gated Director — the hold is unchanged.
        toast.info(`A control token is required to ${held ? 'disconnect' : 'connect'} a timer.`);
        return;
      }
      applyTimer(updated);
      toast.success(held ? `Disconnected “${updated.name}”.` : `Connecting to “${updated.name}”…`);
    } catch (err) {
      // A Mock answers 400 (nothing to dial) — it should never reach here, since the control is
      // only offered for a RotorHazard timer, but say something useful rather than raw HTTP.
      const msg = err instanceof Error ? err.message : String(err);
      if (/\b400\b/.test(msg)) toast.error(`“${timer.name}” has no connection to test.`);
      else toast.error(msg);
    } finally {
      connecting = undefined;
    }
  }

  // ── The add / edit dialog ──────────────────────────────────────────────────
  // One dialog drives both create and edit; `editing` (the timer, or undefined for "add") chooses
  // which protocol call submit makes. Kind-specific fields are kept as strings while the RD types
  // and coerced on submit; switching the kind `Select` keeps both kinds' fields around.
  let formOpen = $state(false);
  let editing = $state<Timer | undefined>(undefined);
  let formName = $state('');
  let formKind = $state<TimerKindTag>('Mock');
  let formLaps = $state(String(DEFAULT_MOCK_LAPS));
  let formLapMs = $state(String(DEFAULT_MOCK_LAP_MS));
  let formUrl = $state('');
  let saving = $state(false);
  let formError = $state<string | undefined>(undefined);

  // ── Channel config (race redesign Slice 4b) ──────────────────────────────────
  // The capability (Fixed | Flexible), node count, and the chosen available channels. The chosen set
  // is held as a `Set<number>` of raw MHz (catalog picks + custom entries); for a Fixed timer it is
  // limited to the timer's built-in allowed set (`Fixed.channels`); a Flexible timer can add custom.
  let formCapability = $state<CapabilityTag>('Flexible');
  let formNodeCount = $state(String(DEFAULT_NODE_COUNT));
  /**
   * The width the Node-count field was **seeded** with, so submit can tell "the RD set a width"
   * from "the RD never touched this field" (#412).
   *
   * `node_count` is an *override*: sending it pins the width, and a pinned width that disagrees
   * with the hardware is precisely the bug #412 was filed for. Before #412 this form always sent
   * it, so editing a timer's name would silently pin its width and undo "follow the timer". Now it
   * is only sent when it actually changed — clearing the override, and per-node enable/disable,
   * live in the Nodes dialog, which is the surface that can express them.
   */
  let seededNodeCount = $state(String(DEFAULT_NODE_COUNT));
  let formChannels = $state<Set<number>>(new Set());
  // A Fixed timer's built-in allowed set (its physically-supported channels); the picker offers
  // exactly these, and `formChannels` is the subset the RD makes available. Empty ⇒ all catalog.
  let formFixedAllowed = $state<number[]>([]);
  let formCustomMhz = $state('');

  /** The catalog entries the picker offers: a Fixed timer's allowed set, else the whole catalog. */
  const offeredBands = $derived.by(() => {
    if (formCapability === 'Flexible' || formFixedAllowed.length === 0) return bands;
    const allowed = new Set(formFixedAllowed);
    return bands
      .map((b) => ({ band: b.band, entries: b.entries.filter((e) => allowed.has(e.mhz)) }))
      .filter((b) => b.entries.length > 0);
  });

  /** Custom (non-catalog) MHz the RD has added — only meaningful for a Flexible timer. */
  const customChannels = $derived(
    [...formChannels].filter((mhz) => !catalog.some((e) => e.mhz === mhz)).sort((a, b) => a - b)
  );

  function resetChannelForm(timer?: Timer) {
    formCapability = capabilityTag(timer?.channel_capability);
    // The EFFECTIVE width (override → reported → fallback), not the raw override: showing a blank
    // or a stale 8 for a timer that is following its hardware would misreport the heat-size cap.
    formNodeCount = String(timer ? timerWidth(timer) : DEFAULT_NODE_COUNT);
    seededNodeCount = formNodeCount;
    formChannels = new Set(timer?.available_channels ?? []);
    formFixedAllowed = fixedAllowed(timer?.channel_capability);
    formCustomMhz = '';
  }

  export function openAdd() {
    editing = undefined;
    formName = '';
    formKind = 'Mock';
    formLaps = String(DEFAULT_MOCK_LAPS);
    formLapMs = String(DEFAULT_MOCK_LAP_MS);
    formUrl = '';
    resetChannelForm();
    formError = undefined;
    formOpen = true;
  }

  function openEdit(timer: Timer) {
    editing = timer;
    formName = timer.name;
    formKind = kindTag(timer.kind);
    if ('Mock' in timer.kind) {
      formLaps = String(timer.kind.Mock.laps);
      formLapMs = String(timer.kind.Mock.lap_ms);
      formUrl = '';
    } else {
      formLaps = String(DEFAULT_MOCK_LAPS);
      formLapMs = String(DEFAULT_MOCK_LAP_MS);
      formUrl = timer.kind.Rotorhazard.url;
    }
    resetChannelForm(timer);
    formError = undefined;
    formOpen = true;
  }

  /** Toggle a catalog channel in/out of the available set. */
  function toggleChannel(mhz: number) {
    const next = new Set(formChannels);
    if (next.has(mhz)) next.delete(mhz);
    else next.add(mhz);
    formChannels = next;
  }

  /**
   * Select or clear a whole band (#429) — the realistic unit of the decision. An RD running
   * Raceband ticks all eight and nothing else; doing that a chip at a time is eight clicks for one
   * choice, on a screen #117 S1 made load-bearing (an empty allowed set now refuses a heat fill).
   *
   * Both of these read `group.entries` — the entries the picker **offered** — so a Fixed timer's
   * band box ticks only what that timer can actually tune, never the raw catalog.
   */
  function toggleBand(group: ChannelBand) {
    formChannels = toggleBandSelection(group.entries, formChannels);
  }

  /** How this band's box reads: checked / unchecked / indeterminate. */
  function bandState(group: ChannelBand) {
    return bandSelection(group.entries, formChannels);
  }

  /** Add a custom raw-MHz channel (Flexible only). Validates a plausible 5.8 GHz centre. */
  function addCustomMhz() {
    // The number Input may bind a number or a string depending on the value typed; coerce safely.
    const mhz = Math.round(Number(String(formCustomMhz).trim()));
    if (!isPlausibleMhz(mhz)) {
      formError = 'Enter a channel frequency between 5300 and 6000 MHz.';
      return;
    }
    formError = undefined;
    formChannels = new Set(formChannels).add(mhz);
    formCustomMhz = '';
  }

  function removeChannel(mhz: number) {
    const next = new Set(formChannels);
    next.delete(mhz);
    formChannels = next;
  }

  /** Enter in the custom-MHz field adds the channel rather than submitting the dialog. */
  function onCustomKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault();
      addCustomMhz();
    }
  }

  /**
   * Build the typed {@link ChannelCapability} the request carries from the form. A **Flexible**
   * timer is `"Flexible"` (any channel). A **Fixed** timer keeps its built-in allowed set when it
   * had one, else its allowed set is exactly the catalog channels the RD chose (no custom).
   */
  function buildCapability(): ChannelCapability {
    if (formCapability === 'Flexible') return 'Flexible';
    // A Fixed timer's allowed set: its pre-existing built-in set, or — if none was declared — the
    // catalog channels the RD picked (custom raw MHz are not allowed on a Fixed timer).
    const allowed =
      formFixedAllowed.length > 0
        ? formFixedAllowed
        : [...formChannels].filter((mhz) => catalog.some((e) => e.mhz === mhz));
    return { Fixed: { channels: allowed } };
  }

  /** The available channels the request carries, ordered by the catalog then custom ascending. */
  function buildAvailable(): number[] {
    // FIRST index wins for a coincident frequency. `new Map(catalog.map(...))` would keep the LAST,
    // and 5880 is both Raceband R7 (index 6) and Fatshark F8 (index 15) — so R7 sorted as if it
    // were F8 and landed after R8. Same rule `channelLabel` documents: the first catalog entry
    // whose MHz matches wins, which is why the picker calls 5880 "Raceband R7" in the first place.
    const order = new Map<number, number>();
    catalog.forEach((e, i) => {
      if (!order.has(e.mhz)) order.set(e.mhz, i);
    });
    return [...formChannels].sort((a, b) => {
      const ai = order.get(a);
      const bi = order.get(b);
      if (ai !== undefined && bi !== undefined) return ai - bi;
      if (ai !== undefined) return -1; // catalog channels before custom
      if (bi !== undefined) return 1;
      return a - b; // both custom: ascending MHz
    });
  }

  /** Build the typed `TimerKind` from the form, or a problem string if a field is invalid. */
  function buildKind(): { kind: TimerKind } | { problem: string } {
    if (formKind === 'Mock') {
      const laps = Number(formLaps);
      const lapMs = Number(formLapMs);
      if (!Number.isFinite(laps) || laps < 1) return { problem: 'Laps must be at least 1.' };
      if (!Number.isFinite(lapMs) || lapMs < 1)
        return { problem: 'Lap pace must be at least 1ms.' };
      return { kind: { Mock: { laps: Math.round(laps), lap_ms: Math.round(lapMs) } } };
    }
    const url = formUrl.trim();
    if (!url) return { problem: 'A RotorHazard URL is required.' };
    return { kind: { Rotorhazard: { url } } };
  }

  async function submitForm(e?: Event) {
    e?.preventDefault();
    if (saving) return;
    const name = formName.trim();
    if (!name) {
      formError = 'A name is required.';
      return;
    }
    const built = buildKind();
    if ('problem' in built) {
      formError = built.problem;
      return;
    }
    const typedNodeCount = Number(formNodeCount);
    if (!Number.isFinite(typedNodeCount) || typedNodeCount < 1) {
      formError = 'Node count must be at least 1.';
      return;
    }
    // #463: never wider than the timer said it is. The `max` on the input stops the spinner, but a
    // typed digit walks straight past it — so the clamp is applied here, and the RD is TOLD what
    // happened rather than watching their number silently change. A never-reported timer (a Mock, a
    // RotorHazard nobody has dialed) has no ceiling: offline setup keeps its free override.
    const clamp = editing
      ? clampNodeCount(editing, Math.round(typedNodeCount))
      : { value: Math.round(typedNodeCount), clamped: false };
    if (clamp.clamped && editing) {
      formNodeCount = String(clamp.value);
      formError = clampExplanation(editing, Math.round(typedNodeCount));
      return;
    }
    const nodeCount = clamp.value;
    const channel_capability = buildCapability();
    const available_channels = buildAvailable();
    // Only carried when the RD actually set a width — see `seededNodeCount`. Omitted otherwise, so
    // an unrelated edit never pins an override the RD did not ask for.
    const node_count = String(formNodeCount) === seededNodeCount ? undefined : nodeCount;
    saving = true;
    formError = undefined;
    try {
      if (editing) {
        const req: UpdateTimerRequest = {
          name,
          kind: built.kind,
          channel_capability,
          node_count,
          available_channels
        };
        const updated = await session.updateTimer(editing.id, req);
        if (!updated) {
          formError = 'A control token is required to edit a timer.';
          return;
        }
        toast.success(`Updated “${updated.name}”.`);
      } else {
        const req: CreateTimerRequest = {
          name,
          kind: built.kind,
          channel_capability,
          node_count,
          available_channels
        };
        const created = await session.createTimer(req);
        if (!created) {
          formError = 'A control token is required to add a timer.';
          return;
        }
        toast.success(`Added “${created.name}”.`);
      }
      formOpen = false;
      await reload();
    } catch (err) {
      formError = err instanceof Error ? err.message : String(err);
    } finally {
      saving = false;
    }
  }

  // ── Node configuration (#412) ───────────────────────────────────────────────
  // The row's node reading is a BUTTON, because it is the only place an RD can see reported vs
  // configured and fix a timer that GridFPV has the wrong width for. The dialog owns the read and
  // the write; this screen only re-folds the timer it hands back, so the row updates without a
  // full reload (which would blank the list under an RD mid-setup).
  let nodesTimer = $state<Timer | undefined>(undefined);
  let nodesOpen = $state(false);

  function openNodes(timer: Timer) {
    nodesTimer = timer;
    nodesOpen = true;
  }

  /**
   * Fold an accepted node write back into the loaded row. The `PUT` answers with a `TimerNodes`
   * view rather than a `Timer`, so the row's three inputs are reconstructed from it — the width
   * override, the observation, and the disabled set (the complement of `enabled`, which is what
   * the registry stores).
   */
  function applyNodes(view: TimerNodes) {
    if (loadState.kind !== 'ready') return;
    const enabled = new Set(view.enabled);
    const disabled_nodes = view.nodes.map((n) => n.node).filter((n) => !enabled.has(n));
    const next = loadState.timers.map((t) =>
      t.id === view.timer
        ? {
            ...t,
            node_count: view.configured ?? undefined,
            reported_nodes: view.reported ?? undefined,
            disabled_nodes
          }
        : t
    );
    loadState = { kind: 'ready', timers: next };
    timers = next;
    onchange?.(next);
  }

  // ── Remove ──────────────────────────────────────────────────────────────────
  let removing = $state<string | undefined>(undefined);
  async function remove(timer: Timer) {
    if (removing) return;
    removing = timer.id;
    try {
      const done = await session.deleteTimer(timer.id);
      if (done === undefined) {
        // The RD cancelled the token prompt on a gated Director — nothing removed.
        toast.info('A control token is required to remove a timer.');
        return;
      }
      toast.success(`Removed “${timer.name}”.`);
      await reload();
    } catch (err) {
      // The built-in Mock answers 400; any other id, a transport/404 — surface it gracefully.
      const msg = err instanceof Error ? err.message : String(err);
      if (/\b400\b/.test(msg)) toast.error('The built-in Mock timer can’t be removed.');
      else toast.error(msg);
    } finally {
      removing = undefined;
    }
  }
</script>

<div class="timer-manager">
  {#if loadState.kind === 'loading'}
    <div class="state-msg" role="status">
      <span class="spinner" aria-hidden="true"></span>
      Loading timers…
    </div>
  {:else if loadState.kind === 'error'}
    <Card elevation="sm">
      <div class="state-error">
        <p>Couldn't load the timers.</p>
        <code>{loadState.message}</code>
        <Button variant="secondary" onclick={load}>Try again</Button>
      </div>
    </Card>
  {:else if loadState.timers.length === 0}
    <div class="empty">
      <p>No timers configured.</p>
      <p class="empty-sub">Add one to give your events a lap source.</p>
    </div>
  {:else}
    {@render listHeader?.()}
    <ul class="timer-list" aria-label="Configured timers">
      {#each displayTimers as timer (timer.id)}
        <li class="timer-row" class:checked={rowChecked?.(timer)}>
          {@render rowLead?.(timer)}
          <div class="timer-main">
            <div class="timer-head">
              <span class="timer-name">{timer.name}</span>
              <Badge tone={kindTone(timer.kind)}>{kindLabel(timer.kind)}</Badge>
              {#if isBuiltInMock(timer)}<Badge tone="neutral" variant="outline">Built-in</Badge
                >{/if}
            </div>
            <span class="timer-sub">{kindSummary(timer.kind)}</span>
            <div class="timer-channels">
              <Badge tone="neutral" variant="outline"
                >{capabilityTag(timer.channel_capability)}</Badge
              >
              <!-- The node reading is the entry point to the node config (#412): this is where an
                   RD sees "reported 4, configured 8" and where they clear it. -->
              <button
                type="button"
                class="nodes"
                title={`Configure the nodes on “${timer.name}” — the heat-size cap`}
                onclick={() => openNodes(timer)}
              >
                {timerNodeSummary(timer)}
              </button>
              {#if timerDrifts(timer)}
                <Badge tone="danger" variant="outline">Timer reports {timer.reported_nodes}</Badge>
              {/if}
              <span class="channel-summary" class:unset={channelsUnset(timer)}
                >{channelSummary(timer)}</span
              >
            </div>
            <!-- The manual-hold readout (#383): only while a hold is up, and only when there is
                 something to say beyond the pill — "Reachable", or what to check when it isn't. -->
            {#if connectionHint(timer)}
              <span class="connect-hint" role="status">{connectionHint(timer)}</span>
            {/if}
            {@render rowNote?.(timer)}
          </div>
          <StatusPill status={timer.status} label={timer.status} size="sm" />
          <!-- `session` is what unlocks the guided install's Restart-timer action (#386);
               without it the callout degrades to chip + guide + download and step 4 points
               at a button that never renders. -->
          <PluginCallout {timer} baseUrl={session.baseUrl} {session} />
          <div class="timer-actions">
            <!-- Connect / Disconnect (#383) — RotorHazard only (a Mock has nothing to dial), and
                 usable with NO active event: the RD tests a URL where timers are configured. -->
            {#if isConnectable(timer)}
              <Button
                variant={isManuallyHeld(timer) ? 'secondary' : 'primary'}
                size="sm"
                loading={connecting === timer.id}
                title={`${connectActionLabel(timer)} “${timer.name}”`}
                onclick={() => toggleConnection(timer)}
              >
                {connectActionLabel(timer)}
              </Button>
            {/if}
            <!-- Tune (#355) — the entry point to the per-timer gate-tuning page. Offered on the
                 timers that HAVE a gate to tune: the built-in Mock has no radio, and a timer this
                 console can't dial has nothing to read. Disabled (not hidden) while disconnected,
                 so the RD learns the action exists and what it needs, rather than wondering where
                 tuning went. -->
            {#if ontune && isConnectable(timer)}
              <Button
                variant="secondary"
                size="sm"
                disabled={!isTimerConnected(timer)}
                title={isTimerConnected(timer)
                  ? `Tune the gate thresholds on “${timer.name}”`
                  : `Connect “${timer.name}” first — tuning reads its live signal.`}
                onclick={() => ontune(timer)}
              >
                Tune
              </Button>
            {/if}
            <Button variant="ghost" size="sm" onclick={() => openEdit(timer)}>Edit</Button>
            {#if !isBuiltInMock(timer)}
              <Button
                variant="danger"
                size="sm"
                loading={removing === timer.id}
                onclick={() => remove(timer)}
              >
                Remove
              </Button>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
    {@render listFooter?.()}
  {/if}
</div>

<!-- The add / edit dialog stacks above whatever embeds the manager. -->
<Dialog bind:open={formOpen} title={editing ? 'Edit timer' : 'Add timer'}>
  <form class="timer-form" onsubmit={submitForm} aria-label={editing ? 'Edit timer' : 'Add timer'}>
    <Field label="Name" error={formError}>
      <Input
        bind:value={formName}
        placeholder="e.g. Mock — fast"
        aria-label="Timer name"
        autocomplete="off"
      />
    </Field>

    <Field label="Kind">
      <Select bind:value={formKind} aria-label="Timer kind">
        <option value="Mock">Mock (synthetic)</option>
        <option value="Rotorhazard">RotorHazard</option>
      </Select>
    </Field>

    {#if formKind === 'Mock'}
      <div class="kind-grid">
        <Field label="Laps" hint="Laps each sim pilot flies.">
          <Input type="number" min="1" step="1" bind:value={formLaps} aria-label="Mock laps" />
        </Field>
        <Field label="Lap pace (ms)" hint="Nominal time for one sim lap.">
          <Input
            type="number"
            min="1"
            step="100"
            bind:value={formLapMs}
            aria-label="Mock lap pace"
          />
        </Field>
      </div>
    {:else}
      <!--
        The URL is dialed verbatim by the RH connector — no trimming, no scheme defaulting — so the
        hint names the exact shape. A trailing slash, a missing scheme, or https against a
        plain-HTTP RH all fail with the same opaque connection error (#381).
      -->
      <Field
        label="URL"
        hint={'The RotorHazard server’s base URL — http://<host>:5000. Dialed exactly as entered: use plain http (not https), and no trailing slash.'}
      >
        <Input
          type="url"
          bind:value={formUrl}
          placeholder="http://rotorhazard.local:5000"
          aria-label="RotorHazard URL"
          autocomplete="off"
        />
      </Field>
    {/if}

    <!-- Channels (race redesign Slice 4b): capability + node count + the available-channels picker. -->
    <hr class="form-rule" />
    <div class="kind-grid">
      <Field
        label="Channel capability"
        hint={formCapability === 'Flexible'
          ? 'Tunes any channel — catalog or custom.'
          : 'Limited to its built-in channels.'}
      >
        <Select bind:value={formCapability} aria-label="Channel capability">
          <option value="Flexible">Flexible (any channel)</option>
          <option value="Fixed">Fixed (built-in set)</option>
        </Select>
      </Field>
      <!-- #463: the ceiling is what the timer reported. `max` stops the spinner and the hint says
           why; a typed digit is caught (and explained) on submit. A never-reported timer gets no
           ceiling at all, so configuring one offline still works. -->
      <Field label="Node count" hint={nodeCountHint(editing)}>
        <Input
          type="number"
          min="1"
          step="1"
          max={editing ? maxNodeCount(editing) : undefined}
          bind:value={formNodeCount}
          aria-label="Node count"
        />
      </Field>
    </div>

    <Field
      label="Available channels"
      hint={formCapability === 'Fixed'
        ? 'Pick from this timer’s built-in channels.'
        : 'Pick catalog channels, and add custom raw-MHz entries below.'}
    >
      <div class="channel-picker" role="group" aria-label="Available channels">
        {#if offeredBands.length === 0}
          <p class="channel-empty" role="status">
            {catalog.length === 0
              ? 'Channel catalog unavailable.'
              : 'This Fixed timer has no built-in channels to offer.'}
          </p>
        {:else}
          {#each offeredBands as group (group.band)}
            <div class="channel-band">
              <!-- Tri-state band box (#429): checked = every offered channel, indeterminate = a
                   partial band (a normal state, not an error), unchecked = none. -->
              <label class="band-head">
                <input
                  type="checkbox"
                  checked={bandState(group) === 'all'}
                  indeterminate={bandState(group) === 'some'}
                  onchange={() => toggleBand(group)}
                  aria-label={`All ${group.band} channels`}
                />
                <span class="band-name">{group.band}</span>
              </label>
              <div class="band-channels">
                {#each group.entries as entry (entry.mhz + '-' + entry.channel)}
                  <label class="channel-chip" class:on={formChannels.has(entry.mhz)}>
                    <input
                      type="checkbox"
                      checked={formChannels.has(entry.mhz)}
                      onchange={() => toggleChannel(entry.mhz)}
                      aria-label={`${group.band} ${entry.channel}, ${entry.mhz} MHz`}
                    />
                    <span class="chip-chan">{entry.channel}</span>
                    <span class="chip-mhz">{entry.mhz}</span>
                  </label>
                {/each}
              </div>
            </div>
          {/each}
        {/if}
      </div>
    </Field>

    {#if formCapability === 'Flexible'}
      <Field label="Custom channel (MHz)" hint="A raw centre frequency not in the catalog.">
        <div class="custom-row">
          <Input
            type="number"
            min="5300"
            max="6000"
            step="1"
            bind:value={formCustomMhz}
            placeholder="e.g. 5685"
            aria-label="Custom channel MHz"
            onkeydown={onCustomKeydown}
          />
          <Button type="button" variant="secondary" onclick={addCustomMhz}>Add</Button>
        </div>
      </Field>
      {#if customChannels.length > 0}
        <div class="custom-list" aria-label="Custom channels">
          {#each customChannels as mhz (mhz)}
            <span class="custom-chip">
              {mhz} MHz
              <button
                type="button"
                class="chip-x"
                onclick={() => removeChannel(mhz)}
                aria-label={`Remove ${mhz} MHz`}>×</button
              >
            </span>
          {/each}
        </div>
      {/if}
    {/if}
  </form>
  {#snippet footer()}
    <Button variant="ghost" onclick={() => (formOpen = false)} disabled={saving}>Cancel</Button>
    <Button variant="primary" onclick={submitForm} loading={saving} disabled={!formName.trim()}>
      {editing ? 'Save changes' : 'Add timer'}
    </Button>
  {/snippet}
</Dialog>

<!-- Node configuration (#412), stacked the same way the add/edit dialog is. Mounted lazily: it
     reads `GET /timers/{id}/nodes` on open, and there is nothing to read until the RD asks. -->
{#if nodesTimer}
  <TimerNodesDialog {session} timer={nodesTimer} bind:open={nodesOpen} onapplied={applyNodes} />
{/if}

<style>
  .timer-manager {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }

  .state-msg {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    padding: var(--gf-space-4) 0;
  }
  .spinner {
    width: 1.1rem;
    height: 1.1rem;
    border-radius: 50%;
    border: 2px solid var(--gf-border);
    border-top-color: var(--gf-accent);
    animation: tm-spin 0.7s linear infinite;
  }
  @keyframes tm-spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation-duration: 2s;
    }
  }
  .state-error {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    align-items: flex-start;
  }
  .state-error p {
    margin: 0;
    font-weight: var(--gf-font-weight-semibold);
  }
  .state-error code {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-danger);
    word-break: break-all;
  }

  .empty {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
    padding: var(--gf-space-5);
    border: 1px dashed var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-surface-alt);
  }
  .empty p {
    margin: 0;
    font-weight: var(--gf-font-weight-medium);
  }
  .empty-sub {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-regular) !important;
  }

  .timer-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .timer-row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-4);
    padding: var(--gf-space-3) var(--gf-space-4);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    background: var(--gf-surface);
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      background var(--gf-motion-fast) var(--gf-ease-out);
  }
  /* When the manager is embedded in select mode, the chosen rows read as "on". */
  .timer-row.checked {
    border-color: var(--gf-accent);
    background: var(--gf-accent-soft);
  }
  .timer-main {
    display: flex;
    flex-direction: column;
    gap: 3px;
    flex: 1;
    min-width: 0;
  }
  .timer-head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  .timer-name {
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .timer-sub {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
  }
  .timer-actions {
    display: flex;
    gap: var(--gf-space-2);
    flex-shrink: 0;
  }
  /* The manual-hold readout (#383) — sits under the row's config lines, sized like real data
     (not chrome) because at a venue this sentence is what the RD is squinting at in sunlight. */
  .connect-hint {
    margin-top: 2px;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }

  .timer-form {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .kind-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--gf-space-3);
  }

  /* ── Channel config (Slice 4b) ───────────────────────────────────────────── */
  .timer-channels {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
    margin-top: 2px;
    /* Actual config data (the assigned channels) — bumped up one step for sunlight readability. */
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  /* The node reading is a control, not a label (#412) — styled to read as the row's other data
     while still being obviously pressable (underline on hover, a real focus ring). */
  .timer-channels .nodes {
    font: inherit;
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-text);
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    text-decoration: underline;
    text-decoration-color: var(--gf-border-strong);
    text-underline-offset: 3px;
  }
  .timer-channels .nodes:hover {
    text-decoration-color: var(--gf-accent);
    color: var(--gf-accent);
  }
  .timer-channels .nodes:focus-visible {
    outline: none;
    border-radius: var(--gf-radius-xs);
    box-shadow: var(--gf-focus-ring);
  }
  .channel-summary {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* An empty allowed set is a gap the RD has to close before a heat can be seated (#117 S1) —
     it reads as a warning, not as neutral config. */
  .channel-summary.unset {
    color: var(--gf-warn);
  }

  .form-rule {
    border: none;
    border-top: 1px solid var(--gf-border-subtle);
    margin: var(--gf-space-2) 0 0;
  }

  .channel-picker {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
    max-height: 18rem;
    overflow-y: auto;
    padding: var(--gf-space-2);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface-alt);
  }
  .channel-empty {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .channel-band {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .band-head {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    align-self: flex-start;
    cursor: pointer;
    user-select: none;
  }
  .band-head input {
    margin: 0;
  }
  .band-name {
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .band-channels {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
  }
  .channel-chip {
    display: inline-flex;
    align-items: baseline;
    gap: var(--gf-space-1);
    padding: var(--gf-space-1) var(--gf-space-2);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface);
    cursor: pointer;
    font-size: var(--gf-font-size-sm);
    user-select: none;
    transition:
      border-color var(--gf-motion-fast) var(--gf-ease-out),
      background var(--gf-motion-fast) var(--gf-ease-out);
  }
  .channel-chip.on {
    border-color: var(--gf-accent);
    background: var(--gf-accent-soft);
  }
  .channel-chip input {
    margin: 0;
  }
  .chip-chan {
    font-weight: var(--gf-font-weight-semibold);
  }
  .chip-mhz {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
    font-variant-numeric: tabular-nums;
  }

  .custom-row {
    display: flex;
    gap: var(--gf-space-2);
    align-items: stretch;
  }
  .custom-row :global(input) {
    flex: 1;
  }
  .custom-list {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
  }
  .custom-chip {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-1);
    padding: var(--gf-space-1) var(--gf-space-2);
    border: 1px solid var(--gf-accent);
    border-radius: var(--gf-radius-pill);
    background: var(--gf-accent-soft);
    font-size: var(--gf-font-size-xs);
    font-variant-numeric: tabular-nums;
  }
  .chip-x {
    border: none;
    background: none;
    cursor: pointer;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-md);
    line-height: 1;
    padding: 0;
  }
  .chip-x:hover {
    color: var(--gf-danger);
  }
</style>
