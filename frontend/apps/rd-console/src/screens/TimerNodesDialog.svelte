<script lang="ts">
  /**
   * TimerNodesDialog — the RD's **node configuration** for one timer (#412).
   *
   * The screen the bench bug had no answer for: a real 4-node NuclearHazard configured as 8, with
   * nothing in the console that showed the disagreement and no way to clear it but a raw `PUT`.
   * It does three things, in the order an RD needs them:
   *
   *  1. **Shows reported alongside configured whenever they differ**, and names the *phantom
   *     nodes* — the enabled seats past what the hardware reported. A pilot seated on one flies
   *     and records nothing, so the notice says exactly that rather than printing two numbers.
   *  2. **Follow the timer** — one click, `PUT { node_count: null }`, which clears the width
   *     override so the hardware's own count is the width again. This is the common repair.
   *  3. **Per node**, not per count. The RD's case is *"reported is 4 but node 3 is busted, I need
   *     to use nodes 1, 2 and 4"* — a set with a hole in it, which a count can never express
   *     because a count only drops nodes off the end.
   *
   * ## The two halves stay apart
   *
   * `reported` is an observation and `configured` is a decision (D27, and #355's calibration
   * drift). Nothing here ever writes an observation into the config: a drift is *shown*, and the
   * RD resolves it. Equally, nothing re-enables a node from `reported` — the Director guarantees a
   * disabled node survives a reconnect, and this screen must not undo that behind the RD's back.
   * That is why the enabled set is only ever sent when the RD presses Save.
   *
   * ## 1-based on screen, 0-based on the wire
   *
   * Every node is rendered by its `TimerNode.label` ("Node 1" for index `0`) — never the raw index
   * and never the `node-{i}` seat ref, both of which are wire handles (the repo display rule).
   * Here that rule has teeth: an off-by-one puts a pilot on a dead gate, which is the failure this
   * whole feature exists to prevent.
   */
  import { Banner, Button, Dialog, toast } from '@gridfpv/components';
  import type { HeatSummary, Timer, TimerNodes } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';
  import {
    driftReading,
    followTimerRequest,
    hasWidthOverride,
    heatOverflow,
    heatOverflowMessage,
    seatSummary
  } from '../lib/timerNodes.js';

  let {
    session,
    timer,
    open = $bindable(false),
    onapplied
  }: {
    session: Session;
    /** The timer being configured — `undefined` closes the dialog. */
    timer: Timer | undefined;
    open?: boolean;
    /** Fires with the saved view after any accepted write, so a host can refresh its own list. */
    onapplied?: (view: TimerNodes) => void;
  } = $props();

  type LoadState =
    | { kind: 'idle' }
    | { kind: 'loading' }
    | { kind: 'error'; message: string }
    | { kind: 'ready'; view: TimerNodes };

  let loadState = $state<LoadState>({ kind: 'idle' });
  /** The RD's working enabled set — 0-based wire indices, only sent on Save. */
  let working = $state<Set<number>>(new Set());
  let saving = $state(false);
  let following = $state(false);
  /** A refusal (or transport failure) from the last write, shown inline rather than as a toast. */
  let writeError = $state<string | undefined>(undefined);
  /** The scheduled heats, when the console is inside an event — the overflow warning's input. */
  let heats = $state<HeatSummary[]>([]);

  const view = $derived(loadState.kind === 'ready' ? loadState.view : undefined);
  const drift = $derived(view ? driftReading(view) : undefined);
  /** The *pending* seat count as the RD ticks boxes — what a heat would actually be capped at. */
  const pendingSeats = $derived(working.size);
  const dirty = $derived.by(() => {
    if (!view) return false;
    if (view.enabled.length !== working.size) return true;
    return view.enabled.some((node) => !working.has(node));
  });
  /** The overflow warning is read against the RD's *pending* set, so it moves as they untick. */
  const overflow = $derived.by(() => {
    if (!view) return undefined;
    const pending: TimerNodes = { ...view, enabled: [...working].sort((a, b) => a - b) };
    return heatOverflow(pending, heats);
  });

  /** (Re)load whenever the dialog opens on a timer. */
  $effect(() => {
    if (!open || !timer) return;
    void load(timer.id);
  });

  async function load(id: string) {
    loadState = { kind: 'loading' };
    writeError = undefined;
    try {
      const v = await session.timerNodes(id);
      seed(v);
    } catch (e) {
      loadState = { kind: 'error', message: e instanceof Error ? e.message : String(e) };
    }
  }

  /** Adopt a server-authoritative view as both the rendered state and the working set. */
  function seed(v: TimerNodes) {
    loadState = { kind: 'ready', view: v };
    working = new Set(v.enabled);
  }

  // The scheduled heats back the "a heat would exceed the enabled set" warning. Only meaningful
  // inside an event; outside one the read is inert and the warning simply never appears.
  $effect(() => {
    if (!open) return;
    session
      .listHeats()
      .then((h) => (heats = h))
      .catch(() => (heats = []));
  });

  function toggle(node: number) {
    const next = new Set(working);
    if (next.has(node)) next.delete(node);
    else next.add(node);
    working = next;
    writeError = undefined;
  }

  /** Clear the width override — the "my bench timer says 4 and GridFPV says 8" repair. */
  async function followTimer() {
    if (!timer || following || saving) return;
    following = true;
    writeError = undefined;
    try {
      const saved = await session.setTimerNodes(timer.id, followTimerRequest());
      if (!saved) {
        writeError = 'A control token is required to change a timer’s nodes.';
        return;
      }
      seed(saved);
      onapplied?.(saved);
      toast.success(`“${timer.name}” now follows the timer’s own node count.`);
    } catch (e) {
      writeError = e instanceof Error ? e.message : String(e);
    } finally {
      following = false;
    }
  }

  /** Send the working set wholesale — never a delta, so a stale console can't half-apply an edit. */
  async function save() {
    if (!timer || saving || following) return;
    saving = true;
    writeError = undefined;
    try {
      const enabled = [...working].sort((a, b) => a - b);
      const saved = await session.setTimerNodes(timer.id, { enabled });
      if (!saved) {
        writeError = 'A control token is required to change a timer’s nodes.';
        return;
      }
      seed(saved);
      onapplied?.(saved);
      toast.success(`Saved the node configuration for “${timer.name}”.`);
    } catch (e) {
      // The Director's refusals ("at least one node must stay enabled") are already phrased for
      // the RD — show them where the RD is working rather than as a disappearing toast.
      writeError = e instanceof Error ? e.message : String(e);
    } finally {
      saving = false;
    }
  }

  function close() {
    open = false;
    loadState = { kind: 'idle' };
    writeError = undefined;
  }
</script>

<Dialog bind:open title={timer ? `Nodes — ${timer.name}` : 'Nodes'} onclose={close}>
  <div class="nodes-body">
    {#if loadState.kind === 'loading' || loadState.kind === 'idle'}
      <p class="state-msg" role="status">Reading this timer’s nodes…</p>
    {:else if loadState.kind === 'error'}
      <Banner tone="danger" title="Couldn’t read the nodes">{loadState.message}</Banner>
      <Button variant="secondary" onclick={() => timer && load(timer.id)}>Try again</Button>
    {:else if view}
      <!-- The drift notice: reported vs configured, whenever they differ, naming the phantom
           nodes by their 1-based labels. A notice, never an edit — the RD resolves it. -->
      {#if drift}
        <Banner tone={drift.tone} title={drift.headline}>
          {drift.detail}
        </Banner>
      {/if}

      <!-- The two values, side by side and always visible, so "what does the hardware say?" is
           answerable without hunting for a warning. -->
      <dl class="width-facts">
        <div class="fact">
          <dt>Timer reports</dt>
          <dd data-testid="reported">
            {view.reported === undefined ? 'Not reported yet' : `${view.reported} nodes`}
          </dd>
        </div>
        <div class="fact">
          <dt>GridFPV uses</dt>
          <dd data-testid="configured">
            {view.width} nodes
            <span class="fact-note">
              {hasWidthOverride(view) ? 'set by you' : 'following the timer'}
            </span>
          </dd>
        </div>
      </dl>

      {#if hasWidthOverride(view)}
        <div class="follow-row">
          <Button variant="secondary" loading={following} onclick={followTimer}>
            Follow the timer
          </Button>
          <span class="follow-note">
            Clears the width you set, so GridFPV uses whatever this timer reports.
          </span>
        </div>
      {/if}

      <!-- Per node, not per count: a dead node is rarely the last one. -->
      <fieldset class="node-set">
        <legend>Enabled nodes</legend>
        <p class="node-hint">
          A disabled node seats no pilot, is offered no channel, and stays disabled when the timer
          reconnects.
        </p>
        <!-- The `<fieldset>` + `<legend>` above already names this group for assistive tech; a
             second explicit role/label here would announce (and match) twice. -->
        <div class="node-grid">
          {#each view.nodes as node (node.node)}
            <label
              class="node-chip"
              class:on={working.has(node.node)}
              class:phantom={!node.reported}
            >
              <input
                type="checkbox"
                checked={working.has(node.node)}
                onchange={() => toggle(node.node)}
                aria-label={node.label}
              />
              <span class="node-name">{node.label}</span>
              {#if !node.reported}
                <span class="node-flag" title="This timer did not report this node">
                  not on the timer
                </span>
              {/if}
            </label>
          {/each}
        </div>
        <p class="seat-summary" data-testid="seat-summary">
          {seatSummary({ ...view, enabled: [...working].sort((a, b) => a - b) })}
        </p>
      </fieldset>

      {#if pendingSeats === 0}
        <Banner tone="warn">
          At least one node must stay enabled — a timer with none caps every heat to no pilots.
        </Banner>
      {:else if overflow}
        <Banner tone="warn" title="A heat would exceed the enabled nodes">
          {heatOverflowMessage(overflow)}
        </Banner>
      {/if}

      {#if writeError}
        <Banner tone="danger" title="The Director refused the change">{writeError}</Banner>
      {/if}
    {/if}
  </div>

  {#snippet footer()}
    <Button variant="ghost" onclick={close} disabled={saving || following}>Close</Button>
    <Button
      variant="primary"
      loading={saving}
      disabled={!dirty || pendingSeats === 0 || following}
      onclick={save}
    >
      Save nodes
    </Button>
  {/snippet}
</Dialog>

<style>
  .nodes-body {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .state-msg {
    margin: 0;
    color: var(--gf-text-muted);
  }

  .width-facts {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--gf-space-3);
    margin: 0;
  }
  .fact {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface-alt);
  }
  .fact dt {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
  }
  .fact dd {
    margin: 0;
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
  }
  .fact-note {
    display: block;
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-normal);
    color: var(--gf-text-muted);
  }

  .follow-row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .follow-note {
    flex: 1;
    min-width: 12rem;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }

  .node-set {
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    padding: var(--gf-space-3);
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .node-set legend {
    padding: 0 var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
  }
  .node-hint {
    margin: 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .node-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(9rem, 1fr));
    gap: var(--gf-space-2);
  }
  .node-chip {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: var(--gf-space-2) var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface);
    cursor: pointer;
    user-select: none;
    flex-wrap: wrap;
  }
  .node-chip.on {
    border-color: var(--gf-accent);
    background: var(--gf-accent-soft);
  }
  /* A node the timer did not report: it exists only because GridFPV is configured wider than the
     hardware. Marked whether it is on or off, because turning it ON is the dangerous act. */
  .node-chip.phantom {
    border-color: var(--gf-danger);
  }
  .node-chip input {
    margin: 0;
  }
  .node-name {
    font-weight: var(--gf-font-weight-medium);
  }
  .node-flag {
    flex-basis: 100%;
    font-size: var(--gf-font-size-xs);
    color: var(--gf-danger);
  }
  .seat-summary {
    margin: 0;
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
  }
</style>
