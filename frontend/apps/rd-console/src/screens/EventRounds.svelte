<script lang="ts">
  /**
   * EventRounds — the **Rounds** half of the in-event "Rounds & Heats" stage (race redesign Slice
   * 2b). The Heats half (filling a round's heats with pilots) lands in Slice 3 and shows here only
   * as a labelled placeholder.
   *
   * A **round** ({@link RoundDef}) is an event-level, class-tagged, *dynamic* format-instance: a
   * named, configured run of one `FormatRegistry` format, scoped to the eligible classes it runs
   * for, with a {@link SeedingRule} deciding how its field is drawn. One eligible class is a *class
   * round*; many/all classes is an *open / practice* round. Rounds are added **as-you-go** — this
   * screen authors them dynamically and the add reflects immediately (the session re-homes
   * `currentEvent` after each write).
   *
   * The RD authors: a `label`; the **eligible classes** (a multi-select of the event's selected
   * classes); the **format** (from `GET /formats`, the engine's single source of truth); the **win
   * condition** ({@link WinCondition}); the **seeding** (From roster, or From ranking — a prior
   * round's top-N, the bracket case the engine consumes in a later slice); and a minimal optional
   * **params** key→value editor. Rounds can be edited (PUT) and removed (DELETE).
   *
   * Field-readable: large text, dark surfaces, consistent with the other stage screens.
   */
  import { Button, Card, Field, Input, Select, toast } from '@gridfpv/components';
  import type {
    Class,
    ClassId,
    NewRoundReq,
    RoundDef,
    RoundId,
    SeedingRule,
    WinCondition
  } from '@gridfpv/types';
  import type { Session } from '../lib/session.svelte.js';

  let { session }: { session: Session } = $props();

  // The app-level class directory (to resolve class ids → names) and the valid format names (the
  // engine's `FormatRegistry::standard()`, via `GET /formats`). Both are open reads, loaded once.
  let classes = $state<Class[]>([]);
  let formats = $state<string[]>([]);

  // The event's rounds, read straight off `currentEvent` (the session re-homes it after each write
  // so this stays live). Display order is definition order.
  const rounds = $derived<RoundDef[]>(session.currentEvent?.rounds ?? []);
  // The event's selected classes — the only classes a round may be eligible for.
  const eventClassIds = $derived<ClassId[]>(session.currentEvent?.classes ?? []);
  const eventClasses = $derived<Class[]>(
    eventClassIds.map((id) => classes.find((c) => c.id === id)).filter((c): c is Class => !!c)
  );

  const className = (id: ClassId): string => classes.find((c) => c.id === id)?.name ?? id;
  const roundLabel = (id: RoundId): string => rounds.find((r) => r.id === id)?.label ?? id;

  $effect(() => {
    session
      .listClasses()
      .then((list) => (classes = list))
      .catch((e) => toast.error(e instanceof Error ? e.message : String(e)));
    session
      .listFormats()
      .then((list) => (formats = list))
      .catch((e) => toast.error(e instanceof Error ? e.message : String(e)));
  });

  // --- The add/edit form -------------------------------------------------------------------------
  // One form drives both add (no `editing`) and edit (an existing round id). The win condition and
  // seeding are kept as discriminator + a couple of numeric knobs, assembled into the wire shapes
  // on submit; params is a free key→value list.

  type WinKind = 'Timed' | 'FirstToLaps' | 'BestLap' | 'BestConsecutive';
  type SeedKind = 'FromRoster' | 'FromRanking';
  interface ParamRow {
    key: string;
    value: string;
  }

  let editing = $state<RoundId | undefined>(undefined);
  let formOpen = $state(false);
  let saving = $state(false);

  let label = $state('');
  let selectedClasses = $state<Set<ClassId>>(new Set());
  let format = $state('');
  let winKind = $state<WinKind>('Timed');
  let winSeconds = $state(120); // Timed window, in seconds (converted to micros on submit).
  let winLaps = $state(3); // FirstToLaps target / BestConsecutive span.
  let seedKind = $state<SeedKind>('FromRoster');
  let seedSource = $state<RoundId | ''>('');
  let seedTopN = $state(8);
  let params = $state<ParamRow[]>([]);

  // Whether the eligible-classes pick reads as open/practice (all selected) or a class round (one).
  const classHint = $derived(
    selectedClasses.size === 0
      ? 'Pick at least one eligible class.'
      : selectedClasses.size === eventClasses.length && eventClasses.length > 1
        ? 'All classes — an open / practice round.'
        : selectedClasses.size === 1
          ? 'One class — a class round.'
          : `${selectedClasses.size} classes eligible.`
  );

  // The other rounds a FromRanking seed may draw from (every round but the one being edited).
  const sourceCandidates = $derived(rounds.filter((r) => r.id !== editing));

  function resetForm() {
    editing = undefined;
    label = '';
    selectedClasses = new Set();
    format = formats[0] ?? '';
    winKind = 'Timed';
    winSeconds = 120;
    winLaps = 3;
    seedKind = 'FromRoster';
    seedSource = '';
    seedTopN = 8;
    params = [];
  }

  export function openAdd() {
    resetForm();
    formOpen = true;
  }

  function openEdit(round: RoundDef) {
    editing = round.id;
    label = round.label;
    selectedClasses = new Set(round.classes);
    format = round.format;
    params = Object.entries(round.params ?? {}).map(([key, value]) => ({ key, value }));

    const wc = round.win_condition;
    if (typeof wc === 'string') {
      winKind = 'BestLap';
    } else if ('Timed' in wc) {
      winKind = 'Timed';
      winSeconds = Math.round(wc.Timed.window_micros / 1_000_000);
    } else if ('FirstToLaps' in wc) {
      winKind = 'FirstToLaps';
      winLaps = wc.FirstToLaps.n;
    } else if ('BestConsecutive' in wc) {
      winKind = 'BestConsecutive';
      winLaps = wc.BestConsecutive.n;
    }

    const seed = round.seeding;
    if (typeof seed === 'string') {
      seedKind = 'FromRoster';
    } else {
      seedKind = 'FromRanking';
      seedSource = seed.FromRanking.source_round;
      seedTopN = seed.FromRanking.top_n;
    }
    formOpen = true;
  }

  function cancel() {
    formOpen = false;
    resetForm();
  }

  function toggleClass(id: ClassId) {
    const next = new Set(selectedClasses);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    selectedClasses = next;
  }

  function addParamRow() {
    params = [...params, { key: '', value: '' }];
  }
  function removeParamRow(i: number) {
    params = params.filter((_, idx) => idx !== i);
  }

  function buildWinCondition(): WinCondition {
    switch (winKind) {
      case 'Timed':
        return { Timed: { window_micros: Math.max(0, Math.round(winSeconds * 1_000_000)) } };
      case 'FirstToLaps':
        return { FirstToLaps: { n: Math.max(1, Math.round(winLaps)) } };
      case 'BestConsecutive':
        return { BestConsecutive: { n: Math.max(1, Math.round(winLaps)) } };
      case 'BestLap':
      default:
        return 'BestLap';
    }
  }

  function buildSeeding(): SeedingRule {
    if (seedKind === 'FromRanking' && seedSource) {
      return {
        FromRanking: { source_round: seedSource, top_n: Math.max(1, Math.round(seedTopN)) }
      };
    }
    return 'FromRoster';
  }

  function buildParams(): { [key: string]: string } {
    const out: { [key: string]: string } = {};
    for (const { key, value } of params) {
      const k = key.trim();
      if (k) out[k] = value;
    }
    return out;
  }

  // The form is submittable once it has a label, at least one eligible class, a format, and — when
  // seeding from a ranking — a chosen source round.
  const canSubmit = $derived(
    label.trim().length > 0 &&
      selectedClasses.size > 0 &&
      format.length > 0 &&
      (seedKind === 'FromRoster' || (seedKind === 'FromRanking' && !!seedSource))
  );

  async function submit() {
    if (saving || !canSubmit) return;
    saving = true;
    // Eligible classes in the event's selection order (a stable, sensible order).
    const req: NewRoundReq = {
      label: label.trim(),
      classes: eventClassIds.filter((id) => selectedClasses.has(id)),
      format,
      params: buildParams(),
      win_condition: buildWinCondition(),
      seeding: buildSeeding()
    };
    try {
      const result = editing
        ? await session.updateRound(editing, req)
        : await session.createRound(req);
      if (!result) {
        toast.info('A control token is required to manage rounds.');
        return;
      }
      toast.success(editing ? 'Round updated.' : 'Round added.');
      formOpen = false;
      resetForm();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      saving = false;
    }
  }

  async function remove(round: RoundDef) {
    try {
      const updated = await session.deleteRound(round.id);
      if (!updated) {
        toast.info('A control token is required to manage rounds.');
        return;
      }
      if (editing === round.id) cancel();
      toast.success('Round removed.');
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    }
  }

  // --- Read-only summaries for the list ----------------------------------------------------------
  function winSummary(wc: WinCondition): string {
    if (typeof wc === 'string') return 'Best lap';
    if ('Timed' in wc) return `Timed · ${Math.round(wc.Timed.window_micros / 1_000_000)}s`;
    if ('FirstToLaps' in wc) return `First to ${wc.FirstToLaps.n} laps`;
    if ('BestConsecutive' in wc) return `Best ${wc.BestConsecutive.n} consecutive`;
    return 'Best lap';
  }

  function seedSummary(seed: SeedingRule): string {
    if (typeof seed === 'string') return 'From roster';
    const { source_round, top_n } = seed.FromRanking;
    return `Top ${top_n} from ${roundLabel(source_round)}`;
  }
</script>

<section class="event-rounds" aria-label="Rounds and heats">
  <Card
    title="Rounds"
    subtitle="Define this event's rounds — eligible classes, format, win condition, and seeding. Rounds are added as you go."
  >
    {#snippet actions()}
      <Button variant="secondary" size="sm" onclick={openAdd} disabled={eventClasses.length === 0}>
        + Add round
      </Button>
    {/snippet}

    {#if eventClasses.length === 0}
      <p class="empty" role="status">
        This event selects no classes yet. Pick classes in the <strong>Classes</strong> stage first —
        a round runs for one or more of them.
      </p>
    {:else if rounds.length === 0}
      <p class="empty" role="status">No rounds yet. Add the first round to get going.</p>
    {/if}

    {#if rounds.length > 0}
      <ol class="round-list">
        {#each rounds as round, i (round.id)}
          <li class="round-row">
            <span class="round-index" aria-hidden="true">{i + 1}</span>
            <div class="round-main">
              <div class="round-head">
                <span class="round-label">{round.label}</span>
                <span class="round-format">{round.format}</span>
              </div>
              <div class="round-meta">
                <span class="meta-chip">
                  {round.classes.length === eventClasses.length && eventClasses.length > 1
                    ? 'All classes'
                    : round.classes.map(className).join(', ') || '—'}
                </span>
                <span class="meta-chip">{winSummary(round.win_condition)}</span>
                <span class="meta-chip">{seedSummary(round.seeding)}</span>
              </div>
            </div>
            <div class="round-actions">
              <Button variant="ghost" size="sm" onclick={() => openEdit(round)}>Edit</Button>
              <Button variant="ghost" size="sm" onclick={() => remove(round)}>Remove</Button>
            </div>
          </li>
        {/each}
      </ol>
    {/if}

    {#if formOpen}
      <form
        class="round-form"
        aria-label={editing ? 'Edit round' : 'Add round'}
        onsubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <h3 class="form-title">{editing ? 'Edit round' : 'New round'}</h3>

        <Field label="Label" required>
          <Input bind:value={label} placeholder="e.g. Qualifying R1" aria-label="Label" />
        </Field>

        <Field label="Eligible classes" required hint={classHint}>
          <div class="class-picker" role="group" aria-label="Eligible classes">
            {#each eventClasses as cls (cls.id)}
              <label class="class-chip">
                <input
                  type="checkbox"
                  checked={selectedClasses.has(cls.id)}
                  onchange={() => toggleClass(cls.id)}
                  aria-label={`Eligible ${cls.name}`}
                />
                <span>{cls.name}</span>
              </label>
            {/each}
          </div>
        </Field>

        <div class="form-grid">
          <Field label="Format" required>
            <Select bind:value={format} aria-label="Format">
              {#each formats as f (f)}
                <option value={f}>{f}</option>
              {/each}
            </Select>
          </Field>

          <Field label="Win condition">
            <Select bind:value={winKind} aria-label="Win condition">
              <option value="Timed">Timed window</option>
              <option value="FirstToLaps">First to N laps</option>
              <option value="BestLap">Best lap</option>
              <option value="BestConsecutive">Best N consecutive</option>
            </Select>
          </Field>

          {#if winKind === 'Timed'}
            <Field label="Window (seconds)">
              <Input type="number" min="1" bind:value={winSeconds} aria-label="Window seconds" />
            </Field>
          {:else if winKind === 'FirstToLaps' || winKind === 'BestConsecutive'}
            <Field label="Laps">
              <Input type="number" min="1" bind:value={winLaps} aria-label="Laps" />
            </Field>
          {/if}
        </div>

        <Field
          label="Seeding"
          hint={seedKind === 'FromRanking'
            ? 'Draw this round from a prior round’s ranking (the bracket / cut case).'
            : 'Draw straight from the eligible classes’ roster membership.'}
        >
          <Select bind:value={seedKind} aria-label="Seeding">
            <option value="FromRoster">From roster</option>
            <option value="FromRanking">From ranking</option>
          </Select>
        </Field>

        {#if seedKind === 'FromRanking'}
          <div class="form-grid">
            <Field label="Source round" required>
              {#if sourceCandidates.length === 0}
                <p class="inline-note">Add another round first to seed from its ranking.</p>
              {:else}
                <Select bind:value={seedSource} aria-label="Source round">
                  <option value="" disabled>Choose a round…</option>
                  {#each sourceCandidates as r (r.id)}
                    <option value={r.id}>{r.label}</option>
                  {/each}
                </Select>
              {/if}
            </Field>
            <Field label="Top N advance">
              <Input type="number" min="1" bind:value={seedTopN} aria-label="Top N" />
            </Field>
          </div>
        {/if}

        <Field
          label="Format params"
          hint="Optional format knobs (e.g. rounds, advance, heat_size)."
        >
          <div class="params">
            {#each params as row, i (i)}
              <div class="param-row">
                <Input bind:value={row.key} placeholder="key" aria-label={`Param ${i + 1} key`} />
                <Input
                  bind:value={row.value}
                  placeholder="value"
                  aria-label={`Param ${i + 1} value`}
                />
                <Button
                  variant="ghost"
                  size="sm"
                  type="button"
                  onclick={() => removeParamRow(i)}
                  aria-label={`Remove param ${i + 1}`}
                >
                  ✕
                </Button>
              </div>
            {/each}
            <Button variant="ghost" size="sm" type="button" onclick={addParamRow}>
              + Add param
            </Button>
          </div>
        </Field>

        <div class="form-actions">
          <Button variant="ghost" type="button" onclick={cancel} disabled={saving}>Cancel</Button>
          <Button variant="primary" type="submit" loading={saving} disabled={!canSubmit}>
            {editing ? 'Save round' : 'Add round'}
          </Button>
        </div>
      </form>
    {/if}
  </Card>

  <Card title="Heats" subtitle="Fill each round’s heats with pilots — coming in Slice 3.">
    <p class="placeholder" role="status">
      <strong>Heat building — Slice 3.</strong> Once a round is defined, this is where its field gets
      drawn into heats and slotted to channels. Not yet available.
    </p>
  </Card>
</section>

<style>
  .event-rounds {
    max-width: 52rem;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .empty,
  .placeholder {
    margin: 0;
    font-size: var(--gf-font-size-md);
    color: var(--gf-text-secondary);
    line-height: 1.5;
  }
  .placeholder strong {
    color: var(--gf-text);
  }

  .round-list {
    list-style: none;
    margin: var(--gf-space-2) 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .round-row {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-3);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
  }
  .round-index {
    flex-shrink: 0;
    width: 1.9rem;
    height: 1.9rem;
    display: grid;
    place-items: center;
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface);
    color: var(--gf-text-muted);
    font-variant-numeric: tabular-nums;
    font-weight: var(--gf-font-weight-semibold);
  }
  .round-main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
  }
  .round-head {
    display: flex;
    align-items: baseline;
    gap: var(--gf-space-2);
  }
  .round-label {
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
    color: var(--gf-text);
  }
  .round-format {
    font-size: var(--gf-font-size-xs);
    color: var(--gf-text-muted);
    font-family: var(--gf-font-mono, monospace);
  }
  .round-meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
  }
  .meta-chip {
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-secondary);
    padding: 0.1rem var(--gf-space-2);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface);
  }
  .round-actions {
    display: flex;
    gap: var(--gf-space-1);
    flex-shrink: 0;
  }

  .round-form {
    margin-top: var(--gf-space-4);
    padding-top: var(--gf-space-4);
    border-top: 1px solid var(--gf-border-subtle);
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
  .form-title {
    margin: 0;
    font-size: var(--gf-font-size-md);
    color: var(--gf-text);
  }
  .form-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(11rem, 1fr));
    gap: var(--gf-space-3);
  }
  .class-picker {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
  }
  .class-chip {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: 0.3rem var(--gf-space-3);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    background: var(--gf-surface-sunken);
    font-size: var(--gf-font-size-md);
    color: var(--gf-text);
    cursor: pointer;
  }
  .class-chip input {
    width: 1.05rem;
    height: 1.05rem;
    accent-color: var(--gf-accent);
    cursor: pointer;
  }
  .params {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .param-row {
    display: grid;
    grid-template-columns: 1fr 1fr auto;
    gap: var(--gf-space-2);
    align-items: center;
  }
  .inline-note {
    margin: 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .form-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--gf-space-2);
    padding-top: var(--gf-space-2);
  }
</style>
