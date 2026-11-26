<script lang="ts">
  /**
   * EventClasses — the classes at this event, each a block that is shut until opened.
   *
   * Shut, a class is one line: its name, how many pilots, and what its standings come from.
   * Opened, it is:
   *
   * - the class's **roster**, to change in place (pick pilots, then Save roster);
   * - the **phases its standings come from**. Grid works these out: every ranked phase
   *   that is for the class, the latest first. The director only moves one up or down, or
   *   says one does not count (docs/primitives.md §5.3);
   * - its **standings** once a phase counts (position, callsign, and the phase that placed
   *   each pilot, by name).
   *
   * A class is added from the class directory with a roster picked from the pilot directory.
   * The standings are the Director's; this only shows them.
   */
  import { Banner, Button, Collapsible } from '@gridfpv/components';
  import type { ClassId, PhaseId, PilotId, RosterView } from '@gridfpv/types';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { standingRows } from '../lib/rulings.js';
  import PilotPicker from './PilotPicker.svelte';

  let { live }: { live: EventLive } = $props();

  const names = $derived(live.names);
  const rosters = $derived(live.view?.rosters ?? []);

  /** Which classes are open. Every class starts shut. */
  let opened = $state<Record<ClassId, boolean>>({});

  /**
   * The roster as it is being changed, by class. A class has an entry only while its roster
   * differs from the Director's, so a change made elsewhere shows at once in a class that
   * is not being edited.
   */
  let drafts = $state<Record<ClassId, PilotId[]>>({});
  let busy = $state<ClassId | undefined>(undefined);
  let errors = $state<Record<ClassId, string | undefined>>({});

  const same = (a: PilotId[], b: PilotId[]) =>
    a.length === b.length && a.every((p, i) => p === b[i]);
  const rosterNow = (roster: RosterView): PilotId[] => drafts[roster.class] ?? roster.pilots;

  function change(roster: RosterView, next: PilotId[]) {
    if (same(next, roster.pilots)) delete drafts[roster.class];
    else drafts[roster.class] = next;
  }

  async function save(roster: RosterView) {
    const pilots = drafts[roster.class];
    if (pilots === undefined || busy !== undefined) return;
    busy = roster.class;
    errors[roster.class] = undefined;
    try {
      if (await live.send({ Record: { RosterSet: { class: roster.class, pilots } } })) {
        delete drafts[roster.class];
      }
    } catch (err) {
      // The Director's own sentence, shown as it is.
      errors[roster.class] = err instanceof Error ? err.message : String(err);
    } finally {
      busy = undefined;
    }
  }

  // ── The phases a class's standings come from ───────────────────────────────
  let phaseBusy = $state(false);
  let phaseErrors = $state<Record<ClassId, string | undefined>>({});

  async function sendFor(roster: RosterView, command: Parameters<EventLive['send']>[0]) {
    if (phaseBusy) return;
    phaseBusy = true;
    phaseErrors[roster.class] = undefined;
    try {
      await live.send(command);
    } catch (err) {
      phaseErrors[roster.class] = err instanceof Error ? err.message : String(err);
    } finally {
      phaseBusy = false;
    }
  }

  /** Move a counted phase one place up (`-1`) or down (`1`) in the order they are read. */
  function move(roster: RosterView, at: number, by: -1 | 1) {
    const phases = [...roster.counted_phases];
    const to = at + by;
    if (to < 0 || to >= phases.length) return;
    [phases[at], phases[to]] = [phases[to], phases[at]];
    void sendFor(roster, { Record: { StandingsSet: { class: roster.class, phases } } });
  }

  /**
   * The "Counts" box was ticked or cleared. The box shows what the Director says, not what
   * was clicked: a change that is refused puts it back.
   */
  async function setCounts(roster: RosterView, phase: PhaseId, box: HTMLInputElement) {
    await sendFor(roster, { Record: { PhaseCounts: { phase, counts: box.checked } } });
    const now = rosters
      .find((each) => each.class === roster.class)
      ?.phases.find((entry) => entry.phase === phase);
    if (now) box.checked = now.counts;
  }

  // The pilot directory may have changed since the page opened: read it when a class is opened.
  function onOpened(id: ClassId, isOpen: boolean) {
    opened[id] = isOpen;
    if (isOpen) void live.reloadPilots();
  }
</script>

{#if rosters.length === 0}
  <p class="muted">No class is in this event yet.</p>
{:else}
  <ul class="classes" aria-label="Classes at this event">
    {#each rosters as roster (roster.class)}
      {@const name = names.klass(roster.class)}
      {@const rows = standingRows(roster, names)}
      {@const dirty = drafts[roster.class] !== undefined}
      <li aria-label={name}>
        <Collapsible
          title={name}
          bind:open={
            () => opened[roster.class] ?? false, (isOpen) => onOpened(roster.class, isOpen)
          }
        >
          {#snippet summary()}
            <span class="count">
              {roster.pilots.length}
              {roster.pilots.length === 1 ? 'pilot' : 'pilots'}
            </span>
            <span class="counted" data-testid="counted">
              {#if roster.counted_phases.length > 0}
                Standings from {roster.counted_phases.map((id) => names.phase(id)).join(', then ')}
              {/if}
            </span>
          {/snippet}

          <form
            class="roster"
            aria-label="Roster of {name}"
            onsubmit={(e) => {
              e.preventDefault();
              void save(roster);
            }}
          >
            <PilotPicker
              pilots={live.pilots}
              bind:chosen={() => rosterNow(roster), (next) => change(roster, next)}
              slotLabel={(index) => `${index + 1}`}
              disabled={busy === roster.class}
            />
            {#if errors[roster.class]}
              <Banner tone="danger">{errors[roster.class]}</Banner>
            {/if}
            {#if dirty}
              <div class="save">
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={busy === roster.class}
                  onclick={() => delete drafts[roster.class]}>Discard</Button
                >
                <Button variant="primary" size="sm" type="submit" disabled={busy === roster.class}>
                  Save roster
                </Button>
              </div>
            {/if}
          </form>

          <section class="from" aria-label="What the standings of {name} come from">
            <h4>Standings come from</h4>
            {#if roster.phases.length === 0}
              <p class="muted" data-testid="no-class-phases">
                No ranked phase is for this class yet. Add one on the Phases page and pick this
                class for it.
              </p>
            {:else}
              {@const counted = roster.phases.filter((entry) => entry.counts).length}
              <ol class="phases" aria-label="Phases of {name}">
                {#each roster.phases as entry, at (entry.phase)}
                  {@const phaseName = names.phase(entry.phase)}
                  <li class:off={!entry.counts} aria-label={phaseName}>
                    <span class="order">{entry.counts ? at + 1 : ''}</span>
                    <span class="phase-name">{phaseName}</span>
                    {#if entry.counts && at === 0 && counted > 1}
                      <span class="muted">decides</span>
                    {/if}
                    <span class="phase-actions">
                      {#if entry.counts && counted > 1}
                        <Button
                          size="sm"
                          variant="ghost"
                          disabled={phaseBusy || at === 0}
                          aria-label="Move {phaseName} up"
                          onclick={() => move(roster, at, -1)}>↑</Button
                        >
                        <Button
                          size="sm"
                          variant="ghost"
                          disabled={phaseBusy || at === counted - 1}
                          aria-label="Move {phaseName} down"
                          onclick={() => move(roster, at, 1)}>↓</Button
                        >
                      {/if}
                      <label class="counts">
                        <input
                          type="checkbox"
                          checked={entry.counts}
                          disabled={phaseBusy}
                          aria-label="{phaseName} counts"
                          onchange={(e) => setCounts(roster, entry.phase, e.currentTarget)}
                        />
                        Counts
                      </label>
                    </span>
                  </li>
                {/each}
              </ol>
            {/if}
            {#if phaseErrors[roster.class]}
              <Banner tone="danger">{phaseErrors[roster.class]}</Banner>
            {/if}
          </section>

          {#if roster.counted_phases.length > 0 && rows.length > 0}
            <table class="standings" aria-label="Standings of {name}">
              <thead>
                <tr>
                  <th scope="col">Position</th>
                  <th scope="col">Pilot</th>
                  <th scope="col">Placed by</th>
                </tr>
              </thead>
              <tbody>
                {#each rows as row (`${row.position}/${row.callsign}`)}
                  <tr>
                    <td class="num">{row.position}</td>
                    <th scope="row">{row.callsign}</th>
                    <td class:muted={row.placedBy === undefined}>
                      {row.placedBy ?? 'Not placed yet'}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        </Collapsible>
      </li>
    {/each}
  </ul>
{/if}

<style>
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .classes {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .count,
  .counted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-regular);
  }
  .counted:not(:empty)::before {
    content: '·';
    margin: 0 var(--gf-space-2);
  }
  .roster {
    display: grid;
    gap: var(--gf-space-3);
  }
  .save {
    display: flex;
    justify-content: flex-end;
    gap: var(--gf-space-2);
  }
  .from {
    margin-top: var(--gf-space-4);
    display: grid;
    gap: var(--gf-space-2);
  }
  .from h4 {
    margin: 0;
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-semibold);
  }
  .phases {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--gf-space-1);
  }
  .phases li {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-1) var(--gf-space-2);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
  }
  /* A phase that does not count is still listed, so it can be counted again. */
  .phases li.off .phase-name {
    color: var(--gf-text-muted);
    text-decoration: line-through;
  }
  .order {
    width: 1.5rem;
    font-family: var(--gf-font-mono);
    color: var(--gf-text-muted);
  }
  .phase-actions {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: var(--gf-space-1);
  }
  .counts {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-1);
    font-size: var(--gf-font-size-sm);
    margin-left: var(--gf-space-2);
  }
  .standings {
    width: 100%;
    margin-top: var(--gf-space-4);
    border-collapse: collapse;
    font-size: var(--gf-font-size-sm);
  }
  .standings th,
  .standings td {
    text-align: left;
    padding: var(--gf-space-1) var(--gf-space-2);
    border-bottom: 1px solid var(--gf-border-subtle);
  }
  .standings thead th {
    font-size: var(--gf-font-size-xs);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-semibold);
  }
  .num {
    font-family: var(--gf-font-mono);
    width: 5rem;
  }
</style>
