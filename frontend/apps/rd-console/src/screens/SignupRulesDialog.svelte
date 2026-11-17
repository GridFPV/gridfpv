<script lang="ts">
  /**
   * SignupRulesDialog — turn pilot sign-up on for a phase, or change its rules
   * (docs/primitives.md §11.5).
   *
   * Every heat of the phase has the same seats, picked here from one timer (or the bench).
   * The rest are the limits: how many heats are open at once, how many spots a pilot may
   * hold, how many runs they get, and whether they must sit heats out in between.
   *
   * Sign-up starts closed to pilots. The director opens it on the sheet when ready.
   */
  import { Banner, Button, Dialog, Field, Input } from '@gridfpv/components';
  import type { Command, PhaseId, SignupRules } from '@gridfpv/types';
  import type { SeatSource } from '../lib/seats.js';
  import { rulesForm, rulesFrom, rulesProblem } from '../lib/signup.js';

  let {
    phase,
    phaseName,
    rules = undefined,
    sources,
    send,
    onclose
  }: {
    phase: PhaseId;
    phaseName: string;
    /** The rules in force. Left out when sign-up is off for the phase. */
    rules?: SignupRules | undefined;
    /** What a heat can be timed by: every timer, then the bench. */
    sources: readonly SeatSource[];
    /** Send one command. Rejects with the Director's reason when it is refused. */
    send: (command: Command) => Promise<unknown>;
    onclose: () => void;
  } = $props();

  // The form is made once, from the rules as the dialog opened.
  // svelte-ignore state_referenced_locally
  let form = $state(rulesForm(rules, sources));
  let busy = $state(false);
  let error = $state<string | undefined>(undefined);
  let tried = $state(false);

  const source = $derived(sources.find((entry) => entry.id === form.source));
  const problem = $derived(tried ? rulesProblem(form) : undefined);

  function pickSource(id: string) {
    form.source = id;
    // Another timer has other seats: start from all of them.
    form.seats = (sources.find((entry) => entry.id === id)?.seats ?? []).map((seat) => seat.seat);
  }
  function toggleSeat(seat: string, on: boolean) {
    form.seats = on ? [...form.seats, seat] : form.seats.filter((other) => other !== seat);
  }

  async function act(command: Command) {
    if (busy) return;
    busy = true;
    error = undefined;
    try {
      await send(command);
      onclose();
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  function save(e: Event) {
    e.preventDefault();
    tried = true;
    if (rulesProblem(form) !== undefined) return;
    void act({ Signup: { Set: { phase, rules: rulesFrom(form, sources) } } });
  }
</script>

<Dialog open={true} title="Pilot sign-up: {phaseName}" {onclose}>
  <form id="signup-rules" aria-label="Sign-up rules" onsubmit={save}>
    <p class="muted">
      Pilots put themselves into heats from their phones. Each heat has the seats you pick here, and
      a seat is the same channel in every heat.
    </p>

    <Field label="Timed by">
      <select
        aria-label="Timed by"
        value={form.source}
        disabled={busy}
        onchange={(e) => pickSource((e.currentTarget as HTMLSelectElement).value)}
      >
        {#each sources as entry (entry.id)}
          <option value={entry.id} disabled={entry.unavailable !== undefined}>
            {entry.name}{entry.unavailable ? ` (${entry.unavailable})` : ''}
          </option>
        {/each}
      </select>
    </Field>

    <fieldset>
      <legend>Seats in every heat</legend>
      {#each source?.seats ?? [] as seat (seat.seat)}
        <label class="seat">
          <input
            type="checkbox"
            checked={form.seats.includes(seat.seat)}
            disabled={busy}
            onchange={(e) => toggleSeat(seat.seat, (e.currentTarget as HTMLInputElement).checked)}
          />
          {seat.label}
        </label>
      {/each}
    </fieldset>

    <div class="numbers">
      <Field label="Heats open at once">
        <Input
          aria-label="Heats open at once"
          inputmode="numeric"
          bind:value={form.openHeats}
          disabled={busy}
        />
      </Field>
      <Field label="Runs for each pilot">
        <Input
          aria-label="Runs for each pilot"
          inputmode="numeric"
          placeholder="No limit"
          bind:value={form.runLimit}
          disabled={busy}
        />
      </Field>
      <Field label="Spots held at once">
        <Input
          aria-label="Spots held at once"
          inputmode="numeric"
          placeholder="No limit"
          bind:value={form.heldLimit}
          disabled={busy}
        />
      </Field>
      <Field label="Heats to sit out between two">
        <Input
          aria-label="Heats to sit out between two"
          inputmode="numeric"
          bind:value={form.rest}
          disabled={busy}
        />
      </Field>
    </div>
    <p class="muted">
      A new heat appears when fewer than that many heats have room. Leave a limit empty for no
      limit. 0 heats to sit out lets a pilot fly back to back.
    </p>

    {#if problem}
      <p class="error" role="alert">{problem}</p>
    {:else if error}
      <Banner tone="danger">{error}</Banner>
    {/if}
  </form>
  {#snippet footer()}
    {#if rules}
      <Button
        variant="ghost"
        disabled={busy}
        title="The heats and who is in them stay as they are"
        onclick={() => act({ Signup: { Set: { phase } } })}>Turn sign-up off</Button
      >
    {/if}
    <Button variant="ghost" onclick={onclose}>Cancel</Button>
    <Button variant="primary" type="submit" form="signup-rules" disabled={busy}>
      {rules ? 'Save' : 'Turn sign-up on'}
    </Button>
  {/snippet}
</Dialog>

<style>
  form {
    display: grid;
    gap: var(--gf-space-3);
  }
  .muted {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  select {
    width: 100%;
    padding: var(--gf-space-2);
    background: var(--gf-surface);
    color: var(--gf-text);
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
    font: inherit;
  }
  fieldset {
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-sm);
    padding: var(--gf-space-2) var(--gf-space-3);
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-2) var(--gf-space-4);
    margin: 0;
  }
  legend {
    font-size: var(--gf-font-size-sm);
    font-weight: var(--gf-font-weight-semibold);
    padding: 0 var(--gf-space-1);
  }
  .seat {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-1);
    font-size: var(--gf-font-size-sm);
  }
  .numbers {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(11rem, 1fr));
    gap: var(--gf-space-3);
  }
  .error {
    margin: 0;
    color: var(--gf-danger);
    font-size: var(--gf-font-size-sm);
  }
</style>
