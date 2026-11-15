<script lang="ts">
  /**
   * EventPage — the open event's Details page: what it is, the lap rules every race in it runs under, and
   * the classes racing in it.
   *
   * - **Details**: date, location, organizer and a description. **Edit** changes them and
   *   the name.
   * - **Lap rules**: the minimum lap time, and how long after the start crossings are
   *   ignored. They are set once here and hold for every race of the event, the ones
   *   already flown included. Each saves when its field is left, and what is shown
   *   afterwards is what the Director has.
   * - **Classes**: each class at the event with its roster and standings.
   *
   * What is raced is on the Phases page; the saved results are on the Results page.
   */
  import { Banner, Button, Card, Dialog, Field, Input } from '@gridfpv/components';
  import type { EventMeta, LapRules } from '@gridfpv/types';
  import Page from '../Page.svelte';
  import type { EventLive } from '../lib/eventLive.svelte.js';
  import { isHeat } from '../lib/phases.js';
  import { formatHash } from '../lib/route.js';
  import type { Session } from '../lib/session.svelte.js';
  import ClassRosterDialog from './ClassRosterDialog.svelte';
  import EventClasses from './EventClasses.svelte';

  let {
    session,
    event,
    live
  }: {
    session: Session;
    event: EventMeta;
    /** The live competition of this event. */
    live: EventLive;
  } = $props();

  const say = (e: unknown): string => (e instanceof Error ? e.message : String(e));
  const SECOND = 1_000_000;

  const view = $derived(live.view);
  const details = $derived([
    { label: 'Date', value: event.date },
    { label: 'Location', value: event.location },
    { label: 'Organizer', value: event.organizer },
    { label: 'Description', value: event.description }
  ]);
  /** What the event holds, in a few numbers. */
  const counts = $derived.by(() => {
    if (!view) return [];
    const pilots = new Set(view.rosters.flatMap((roster) => roster.pilots));
    return [
      { label: 'Classes', value: view.rosters.length },
      { label: 'Pilots in them', value: pilots.size },
      { label: 'Heats', value: view.phases.filter((phase) => isHeat(phase.def)).length },
      { label: 'Saved races', value: view.races.filter((race) => race.state === 'Saved').length }
    ];
  });

  // ── Lap rules ──────────────────────────────────────────────────────────────
  // Each field shows what the Director has until it is typed in. Leaving it sends the pair.
  // An emptied number field binds `null`; it is kept as `''` so it reads as "typed, and
  // empty" (no rule) rather than "not typed" (which would show the Director's value again).
  let typed = $state<{ minLap?: number | string; ignore?: number | string }>({});
  let rulesBusy = $state(false);
  let rulesError = $state<string | undefined>(undefined);
  let rulesSaved = $state(false);
  const seconds = (micros: number | undefined): number | string =>
    micros === undefined ? '' : micros / SECOND;
  const minLap = $derived(typed.minLap ?? seconds(view?.lap_rules.min_lap_micros));
  const ignore = $derived(typed.ignore ?? seconds(view?.lap_rules.ignore_before_micros));

  /** Seconds from a field as microseconds. Empty or 0 is "none". `null` is not a time. */
  function micros(value: number | string): number | undefined | null {
    if (typeof value === 'string' && value.trim() === '') return undefined;
    const n = Number(value);
    if (!Number.isFinite(n) || n < 0) return null;
    return n === 0 ? undefined : Math.round(n * SECOND);
  }

  async function saveRules() {
    if (rulesBusy || !view) return;
    const min = micros(minLap);
    const ign = micros(ignore);
    rulesSaved = false;
    if (min === null || ign === null) {
      rulesError = 'A lap rule is a number of seconds, 0 or more.';
      return;
    }
    const rules: LapRules = {
      ...(min === undefined ? {} : { min_lap_micros: min }),
      ...(ign === undefined ? {} : { ignore_before_micros: ign })
    };
    const now = view.lap_rules;
    if (
      rules.min_lap_micros === now.min_lap_micros &&
      rules.ignore_before_micros === now.ignore_before_micros
    ) {
      typed = {};
      rulesError = undefined;
      return;
    }
    rulesBusy = true;
    rulesError = undefined;
    try {
      if (await live.send({ Record: { LapRulesSet: { rules } } })) {
        typed = {};
        rulesSaved = true;
      }
    } catch (e) {
      rulesError = say(e);
    } finally {
      rulesBusy = false;
    }
  }

  // ── Edit the details ───────────────────────────────────────────────────────
  let editOpen = $state(false);
  let edit = $state({ name: '', date: '', location: '', organizer: '', description: '' });
  let editBusy = $state(false);
  let editError = $state<string | undefined>(undefined);

  function openEdit() {
    edit = {
      name: event.name,
      date: event.date ?? '',
      location: event.location ?? '',
      organizer: event.organizer ?? '',
      description: event.description ?? ''
    };
    editError = undefined;
    editOpen = true;
  }

  async function submitEdit(e?: Event) {
    e?.preventDefault();
    if (editBusy || !edit.name.trim()) return;
    editBusy = true;
    editError = undefined;
    try {
      if (await session.updateEvent(event.id, edit)) editOpen = false;
    } catch (err) {
      editError = say(err);
    } finally {
      editBusy = false;
    }
  }

  let addClassOpen = $state(false);
</script>

<Page
  title={event.name}
  help="event"
  lede="What this event is, the lap rules its races run under, and who is racing in it."
>
  {#snippet actions()}
    <Button variant="secondary" onclick={openEdit}>Edit</Button>
  {/snippet}

  {#if live.loadError}
    <Banner tone="danger" title="The event could not be read.">{live.loadError}</Banner>
  {/if}

  <div class="two">
    <Card title="Details" elevation="sm">
      <dl class="facts" aria-label="Event details">
        {#each details as row (row.label)}
          <div>
            <dt>{row.label}</dt>
            <dd class:unset={!row.value?.trim()}>{row.value?.trim() || '—'}</dd>
          </div>
        {/each}
      </dl>
    </Card>
    <Card title="In this event" elevation="sm">
      {#if !view}
        {#if !live.loadError}<p class="muted" role="status">Loading…</p>{/if}
      {:else}
        <dl class="facts" aria-label="In this event">
          {#each counts as row (row.label)}
            <div>
              <dt>{row.label}</dt>
              <dd>{row.value}</dd>
            </div>
          {/each}
        </dl>
        <p class="muted">
          Heats are added on the
          <a href={formatHash({ kind: 'event', section: 'phases' })}>Phases page</a>. Pilots,
          classes, formats and seat profiles are kept outside the event and carry over.
        </p>
      {/if}
    </Card>
  </div>

  {#if view}
    <Card
      title="Lap rules"
      subtitle="Set once for the whole event. They apply to every race in it, flown or still to fly."
      elevation="sm"
    >
      <form
        class="rules"
        aria-label="Lap rules"
        onsubmit={(e) => {
          e.preventDefault();
          void saveRules();
        }}
      >
        <Field
          label="Minimum lap time (seconds)"
          hint="A crossing sooner than this after the pilot's last one is not counted as a lap. It stays in the race, and Marshal can count it. Empty or 0 means no minimum."
        >
          <Input
            type="number"
            inputmode="decimal"
            min="0"
            step="0.1"
            aria-label="Minimum lap time, in seconds"
            bind:value={
              () => minLap, (value) => ((typed.minLap = value ?? ''), (rulesSaved = false))
            }
            onblur={() => void saveRules()}
            disabled={rulesBusy}
          />
        </Field>
        <Field
          label="Ignore crossings before (seconds)"
          hint="Crossings in the first seconds after the start are not counted: a take-off right under the gate. Crossings before the start never count. Empty or 0 ignores nothing after it."
        >
          <Input
            type="number"
            inputmode="decimal"
            min="0"
            step="0.1"
            aria-label="Ignore crossings before, in seconds"
            bind:value={
              () => ignore, (value) => ((typed.ignore = value ?? ''), (rulesSaved = false))
            }
            onblur={() => void saveRules()}
            disabled={rulesBusy}
          />
        </Field>
        <!-- Enter in a field saves, as leaving it does. -->
        <button type="submit" class="hidden-submit" tabindex="-1" aria-hidden="true"></button>
      </form>
      <p class="rules-state" role="status" data-testid="lap-rules-state">
        {#if rulesError}
          <span class="bad">{rulesError}</span>
        {:else if rulesBusy}
          Saving…
        {:else if rulesSaved}
          Saved. The Director has these rules.
        {/if}
      </p>
      <p class="muted">
        A timer also has a lap cooldown of its own, set on the
        <a href={formatHash({ kind: 'seats' })}>Seats page</a>. It swallows the bounce of one pass
        at the timer. Keep it shorter than the minimum lap time here.
      </p>
    </Card>

    <Card title="Classes" elevation="sm">
      {#snippet actions()}
        <Button size="sm" variant="secondary" onclick={() => (addClassOpen = true)}>
          + Add class
        </Button>
      {/snippet}
      <EventClasses {live} />
    </Card>
  {/if}
</Page>

{#if addClassOpen}
  <ClassRosterDialog {live} onclose={() => (addClassOpen = false)} />
{/if}

<Dialog bind:open={editOpen} title="Edit the event" onclose={() => (editError = undefined)}>
  <form class="edit" aria-label="Edit the event" onsubmit={submitEdit}>
    <Field label="Event name" required error={editError}>
      <Input bind:value={edit.name} aria-label="Event name" autocomplete="off" />
    </Field>
    <Field label="Date">
      <Input type="date" bind:value={edit.date} aria-label="Event date" />
    </Field>
    <Field label="Location">
      <Input bind:value={edit.location} aria-label="Event location" autocomplete="off" />
    </Field>
    <Field label="Organizer">
      <Input bind:value={edit.organizer} aria-label="Event organizer" autocomplete="off" />
    </Field>
    <Field label="Description">
      <Input bind:value={edit.description} aria-label="Event description" autocomplete="off" />
    </Field>
  </form>
  {#snippet footer()}
    <Button variant="ghost" onclick={() => (editOpen = false)} disabled={editBusy}>Cancel</Button>
    <Button variant="primary" onclick={() => submitEdit()} disabled={editBusy || !edit.name.trim()}>
      {editBusy ? 'Saving…' : 'Save'}
    </Button>
  {/snippet}
</Dialog>

<style>
  .two {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(18rem, 1fr));
    gap: var(--gf-space-4);
  }
  .facts {
    margin: 0;
    display: grid;
    gap: var(--gf-space-2);
  }
  .facts div {
    display: grid;
    grid-template-columns: 8rem 1fr;
    gap: var(--gf-space-3);
    align-items: baseline;
  }
  .facts dt {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .facts dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .facts dd.unset {
    color: var(--gf-text-faint);
  }
  .muted {
    margin: var(--gf-space-3) 0 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .muted a {
    color: var(--gf-accent);
  }
  .rules {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr));
    gap: var(--gf-space-4);
  }
  .hidden-submit {
    display: none;
  }
  .rules-state {
    margin: var(--gf-space-2) 0 0;
    min-height: 1.2em;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-success);
  }
  .rules-state .bad {
    color: var(--gf-danger);
  }
  .edit {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-4);
  }
</style>
