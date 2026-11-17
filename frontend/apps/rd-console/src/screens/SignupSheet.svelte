<script lang="ts">
  /**
   * SignupSheet — the director's view of one sign-up phase (docs/primitives.md §11.5).
   *
   * A row for each heat, a column for each seat, and who is where. From here the director
   * opens and closes sign-up, closes one heat to pilots, puts a pilot on a seat, drops one
   * (and says whether it counts as a run), pulls one forward to fill a thin heat, and
   * gives runs: one more for everyone, or more or fewer for one pilot.
   *
   * Every change is one command. The sheet shown afterwards is the Director's.
   */
  import { Badge, Banner, Button, Card, Dialog } from '@gridfpv/components';
  import type { Command, PilotId, SeatChannel, SignupHeatView } from '@gridfpv/types';
  import type { Names } from '../lib/names.js';
  import {
    heatStateWords,
    pullTarget,
    runsLeft,
    seatHeadings,
    type SignupPhase
  } from '../lib/signup.js';

  let {
    phase,
    names,
    channels = [],
    send,
    onrules
  }: {
    phase: SignupPhase;
    names: Names;
    channels?: readonly SeatChannel[];
    /** Send one command. Rejects with the Director's reason when it is refused. */
    send: (command: Command) => Promise<unknown>;
    /** Open the rules dialog. */
    onrules: () => void;
  } = $props();

  const signup = $derived(phase.signup);
  const rules = $derived(signup.rules);
  const seats = $derived(seatHeadings(rules, names, channels));
  const editable = $derived(!phase.locked);

  let busy = $state(false);
  let error = $state<string | undefined>(undefined);
  /** The seat a pilot is being picked for. */
  let adding = $state<{ heat: SignupHeatView; seat: number } | undefined>(undefined);
  /** The pilot being dropped, while the page asks whether it counts. */
  let dropping = $state<{ heat: SignupHeatView; pilot: PilotId } | undefined>(undefined);

  async function act(command: Command): Promise<boolean> {
    if (busy) return false;
    busy = true;
    error = undefined;
    try {
      await send(command);
      return true;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
      return false;
    } finally {
      busy = false;
    }
  }

  const setOpen = (open: boolean) =>
    act({ Signup: { Set: { phase: phase.id, rules: { ...rules, open } } } });
  const oneMoreEach = () =>
    act({
      Signup: {
        Set: { phase: phase.id, rules: { ...rules, run_limit: (rules.run_limit ?? 0) + 1 } }
      }
    });
  const closeHeat = (heat: SignupHeatView, closed: boolean) =>
    act({ Signup: { CloseHeat: { heat: heat.heat, closed } } });
  const setExtra = (pilot: PilotId, extra: number) =>
    act({ Signup: { SetRuns: { phase: phase.id, pilot, extra } } });

  async function add(pilot: PilotId) {
    const target = adding;
    if (!target) return;
    const done = await act({
      Signup: {
        Take: { heat: target.heat.heat, seat: rules.seats[target.seat], pilot, by: 'Director' }
      }
    });
    if (done) adding = undefined;
  }
  async function drop(counted: boolean) {
    const target = dropping;
    dropping = undefined;
    if (!target) return;
    await act({ Signup: { Drop: { heat: target.heat.heat, pilot: target.pilot, counted } } });
  }
  const pull = (pilot: PilotId, from: SignupHeatView, to: SignupHeatView) =>
    act({ Signup: { Pull: { pilot, from: from.heat, to: to.heat } } });

  /** Whether the director can still change who is in a heat. */
  const canEdit = (heat: SignupHeatView) =>
    editable && (heat.state === 'Open' || heat.state === 'Closed');
  /** The pilots who could be put in a heat: everyone on the sheet who is not in it. */
  const candidates = (heat: SignupHeatView) =>
    signup.pilots
      .filter((entry) => !heat.seats.includes(entry.pilot))
      .map((entry) => ({ id: entry.pilot, callsign: names.pilot(entry.pilot) }))
      .sort((a, b) => a.callsign.localeCompare(b.callsign));
  const pilotRows = $derived(
    signup.pilots
      .map((entry) => ({ ...entry, callsign: names.pilot(entry.pilot) }))
      .sort((a, b) => a.callsign.localeCompare(b.callsign))
  );
  const heatName = (heat: SignupHeatView) => names.phase(heat.heat);
</script>

<Card title="Sign-up: {phase.name}" elevation="sm">
  {#snippet actions()}
    <Badge tone={rules.open ? 'success' : 'neutral'}>
      <span data-testid="signup-open">{rules.open ? 'Open to pilots' : 'Closed to pilots'}</span>
    </Badge>
    {#if editable}
      <Button
        size="sm"
        variant={rules.open ? 'secondary' : 'primary'}
        disabled={busy}
        onclick={() => setOpen(!rules.open)}
      >
        {rules.open ? 'Close sign-up' : 'Open sign-up'}
      </Button>
      <Button size="sm" variant="ghost" onclick={onrules}>Rules</Button>
    {/if}
  {/snippet}

  {#if error}
    <Banner tone="danger">{error}</Banner>
  {/if}
  {#if phase.locked}
    <p class="muted">This phase is locked, so the sheet cannot be changed.</p>
  {/if}

  <div class="scroll">
    <table aria-label="Heats of {phase.name}">
      <thead>
        <tr>
          <th scope="col">Heat</th>
          {#each seats as seat (seat.name)}
            <th scope="col">
              {seat.name}
              {#if seat.channel}<span class="channel">{seat.channel}</span>{/if}
            </th>
          {/each}
          <th scope="col"><span class="visually-hidden">Heat actions</span></th>
        </tr>
      </thead>
      <tbody>
        {#each signup.heats as heat (heat.heat)}
          <tr data-state={heat.state}>
            <th scope="row">
              {heat.number}
              {#if heatStateWords(heat)}
                <Badge tone={heat.state === 'Racing' ? 'success' : 'neutral'}>
                  {heatStateWords(heat)}
                </Badge>
              {/if}
            </th>
            {#each heat.seats as on, index (index)}
              <td>
                {#if on !== null && on !== undefined}
                  {@const who = names.pilot(on)}
                  {@const target = pullTarget(signup, heat, index)}
                  <span class="who">{who}</span>
                  {#if canEdit(heat)}
                    <span class="cell-actions">
                      {#if target}
                        <Button
                          size="sm"
                          variant="ghost"
                          disabled={busy}
                          title="Move {who} forward to heat {target.number}, on the same seat"
                          aria-label="Pull {who} forward to heat {target.number}"
                          onclick={() => pull(on, heat, target)}>↑ {target.number}</Button
                        >
                      {/if}
                      <Button
                        size="sm"
                        variant="ghost"
                        disabled={busy}
                        aria-label="Drop {who} from {heatName(heat)}"
                        onclick={() => (dropping = { heat, pilot: on })}>Drop</Button
                      >
                    </span>
                  {/if}
                {:else if canEdit(heat)}
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    aria-label="Add a pilot to {seats[index].name} of {heatName(heat)}"
                    onclick={() => (adding = { heat, seat: index })}>+ Add</Button
                  >
                {:else}
                  <span class="muted">—</span>
                {/if}
              </td>
            {/each}
            <td class="row-actions">
              {#if editable && heat.state === 'Open'}
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  title="Pilots can no longer join or leave this heat from a phone. You still can."
                  aria-label="Close {heatName(heat)} to pilots"
                  onclick={() => closeHeat(heat, true)}>Close to pilots</Button
                >
              {:else if editable && heat.state === 'Closed'}
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  aria-label="Open {heatName(heat)} to pilots"
                  onclick={() => closeHeat(heat, false)}>Open to pilots</Button
                >
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>

  <h3>Runs</h3>
  {#if editable && rules.run_limit !== undefined && rules.run_limit !== null}
    <p class="runs-head">
      Each pilot gets {rules.run_limit}.
      <Button size="sm" variant="secondary" disabled={busy} onclick={oneMoreEach}>
        One more run for everyone
      </Button>
    </p>
  {:else if rules.run_limit === undefined || rules.run_limit === null}
    <p class="muted">No limit on runs. Set one under Rules.</p>
  {/if}
  <div class="scroll">
    <table aria-label="Runs of each pilot in {phase.name}">
      <thead>
        <tr>
          <th scope="col">Pilot</th>
          <th scope="col" class="num">Flown</th>
          <th scope="col" class="num">Held</th>
          <th scope="col" class="num">Left</th>
          <th scope="col"><span class="visually-hidden">Change runs</span></th>
        </tr>
      </thead>
      <tbody>
        {#each pilotRows as row (row.pilot)}
          <tr>
            <th scope="row">{row.callsign}</th>
            <td class="num">{row.used}</td>
            <td class="num">{row.held}</td>
            <td class="num">{runsLeft(row) ?? '—'}</td>
            <td class="row-actions">
              {#if editable && rules.run_limit !== undefined && rules.run_limit !== null}
                {#if row.extra !== 0}
                  <span class="muted" data-testid="extra">
                    {row.extra > 0 ? `+${row.extra}` : row.extra}
                  </span>
                {/if}
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  aria-label="One run fewer for {row.callsign}"
                  onclick={() => setExtra(row.pilot, row.extra - 1)}>−</Button
                >
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  aria-label="One more run for {row.callsign}"
                  onclick={() => setExtra(row.pilot, row.extra + 1)}>+</Button
                >
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
</Card>

{#if adding}
  {@const target = adding}
  <Dialog
    open={true}
    title="Add a pilot: {heatName(target.heat)}, {seats[target.seat].name}"
    onclose={() => (adding = undefined)}
  >
    {#if candidates(target.heat).length === 0}
      <p class="muted">Every pilot in this phase is already in this heat.</p>
    {:else}
      <ul class="pick" aria-label="Pilots to add">
        {#each candidates(target.heat) as pilot (pilot.id)}
          <li>
            <Button variant="secondary" disabled={busy} onclick={() => add(pilot.id)}>
              {pilot.callsign}
            </Button>
          </li>
        {/each}
      </ul>
    {/if}
    {#if error}<Banner tone="danger">{error}</Banner>{/if}
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (adding = undefined)}>Cancel</Button>
    {/snippet}
  </Dialog>
{/if}

{#if dropping}
  {@const target = dropping}
  <Dialog
    open={true}
    title="Drop {names.pilot(target.pilot)} from {heatName(target.heat)}?"
    onclose={() => (dropping = undefined)}
  >
    <p>Does it use up one of their runs?</p>
    <p class="muted">
      Pick "Does not count" for a pilot who never got to fly, or had video trouble. Pick "Counts" to
      use the run up anyway.
    </p>
    {#snippet footer()}
      <Button variant="ghost" onclick={() => (dropping = undefined)}>Cancel</Button>
      <Button variant="secondary" onclick={() => drop(true)}>Counts as a run</Button>
      <Button variant="primary" onclick={() => drop(false)}>Does not count</Button>
    {/snippet}
  </Dialog>
{/if}

<style>
  .scroll {
    overflow-x: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--gf-font-size-sm);
  }
  th,
  td {
    text-align: left;
    padding: var(--gf-space-1) var(--gf-space-2);
    border-bottom: 1px solid var(--gf-border-subtle);
    vertical-align: middle;
    white-space: nowrap;
  }
  thead th {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-semibold);
  }
  tr[data-state='Flown'] td,
  tr[data-state='Flown'] th {
    color: var(--gf-text-muted);
  }
  .channel {
    display: block;
    text-transform: none;
    letter-spacing: 0;
    font-family: var(--gf-font-mono);
  }
  .who {
    font-weight: var(--gf-font-weight-semibold);
  }
  .cell-actions {
    margin-left: var(--gf-space-1);
  }
  .row-actions {
    text-align: right;
  }
  .num {
    text-align: right;
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
  }
  h3 {
    margin: var(--gf-space-4) 0 var(--gf-space-2);
    font-size: var(--gf-font-size-sm);
  }
  p {
    margin: 0 0 var(--gf-space-2);
  }
  .runs-head {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    flex-wrap: wrap;
  }
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .pick {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
  }
  .visually-hidden {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
