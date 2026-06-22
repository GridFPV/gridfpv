<script lang="ts">
  /**
   * EventSetupWizard — the guided first-pass over the event's stage-pages (race redesign Slice 7).
   *
   * There is **no "Setup" tab**: the event workspace is the editable stage-pages (Classes, Roster,
   * Timers, Rounds, …). This wizard is a guided first-pass **overlay** that walks a new event through
   * those stages **reusing the SAME components and commands the pages use** — there is no separate
   * config bag. Everything it sets is persisted by the embedded stage component itself (via
   * `setEventClasses` / `setEventRoster` / `setClassMembership` / `setEventTimers` / `createRound`),
   * so it all remains fully editable on the pages afterward. The wizard is optional and re-runnable.
   *
   * It is launched on **creating a new event** (the Events page offers "Set up event") and from a
   * **"Setup wizard"** button in the event workspace header. Each step **embeds the actual stage
   * component** inside a stepper frame (Back / Next / Skip + a progress indicator) — one source of
   * truth per step. Steps are skippable (everything stays editable later); the final Review shows a
   * gentle "ready to race?" summary read straight off the live event (classes ≥1, ≥1 member, a
   * timer, ≥1 round).
   *
   * Field-readable (large text, dark) and consistent with the stage screens it embeds.
   */
  import { Button } from '@gridfpv/components';
  import type { Session } from '../lib/session.svelte.js';
  import EventClasses from './EventClasses.svelte';
  import EventRoster from './EventRoster.svelte';
  import EventTimers from './EventTimers.svelte';
  import EventRounds from './EventRounds.svelte';

  let {
    session,
    open = $bindable(false),
    onclose = undefined
  }: {
    session: Session;
    open?: boolean;
    onclose?: () => void;
  } = $props();

  // The ordered steps. Each reuses an existing stage component (or, for Review, reads the live
  // event) — the wizard is pure orchestration, it reimplements none of the stage logic.
  type StepId = 'classes' | 'roster' | 'timers' | 'rounds' | 'review';
  const STEPS: { id: StepId; label: string; blurb: string }[] = [
    {
      id: 'classes',
      label: 'Classes',
      blurb: 'Pick the classes this event runs. New classes you add here join the directory.'
    },
    {
      id: 'roster',
      label: 'Roster',
      blurb: 'Mark who is present, then place pilots into each class. Add new pilots inline.'
    },
    {
      id: 'timers',
      label: 'Timer & channels',
      blurb: 'Choose the timer(s) that feed this event and confirm their channels / node count.'
    },
    {
      id: 'rounds',
      label: 'First round',
      blurb: 'Define one round so the event is ready to fill heats. More can be added later.'
    },
    {
      id: 'review',
      label: 'Review',
      blurb: 'A quick "ready to race?" check. Everything here stays editable on the pages.'
    }
  ];

  let step = $state(0);
  const current = $derived(STEPS[step]);
  const isFirst = $derived(step === 0);
  const isLast = $derived(step === STEPS.length - 1);

  // Each time the overlay opens, start from the first step.
  let wasOpen = false;
  $effect(() => {
    if (open && !wasOpen) step = 0;
    wasOpen = open;
  });

  // The embedded Rounds stage exposes `openAdd()` — when the wizard lands on the First-round step we
  // pop its add-round form open **once** so the RD is dropped straight into authoring one (still the
  // same form + `createRound` the page uses; nothing reimplemented). The `autoOpened` guard ensures
  // we only auto-open on entry, never re-running and wiping what the RD has typed into the form.
  let roundsStage = $state<EventRounds | undefined>(undefined);
  let autoOpened = false;
  $effect(() => {
    if (open && current.id === 'rounds' && roundsStage) {
      if (!autoOpened) {
        autoOpened = true;
        roundsStage.openAdd();
      }
    } else {
      autoOpened = false;
    }
  });

  function back() {
    if (step > 0) step -= 1;
  }
  function next() {
    if (step < STEPS.length - 1) step += 1;
  }
  function skip() {
    next();
  }
  function finish() {
    open = false;
    onclose?.();
  }
  function onKeydown(e: KeyboardEvent) {
    if (open && e.key === 'Escape') {
      e.stopPropagation();
      finish();
    }
  }

  // ── Readiness summary (the "ready to race?" gate) ──────────────────────────
  // Read straight off the live event the embedded stages persist to — no separate config to track.
  // A check is "met" once the corresponding stage has written something; none of them is required
  // to close (everything stays editable later), so this is a gentle summary, not a hard gate.
  const ev = $derived(session.currentEvent);
  const classCount = $derived(ev?.classes?.length ?? 0);
  const memberCount = $derived(
    (ev?.classes_membership ?? []).reduce((set, m) => {
      // Membership entries are member slots (`{ pilot, channel? }`) — Slice 7a; count distinct pilots.
      for (const s of m.pilots) set.add(s.pilot);
      return set;
    }, new Set<string>()).size
  );
  const timerCount = $derived(ev?.timers?.length ?? 0);
  const roundCount = $derived(ev?.rounds?.length ?? 0);

  const checks = $derived([
    {
      label: 'At least one class selected',
      met: classCount >= 1,
      detail: `${classCount} selected`
    },
    {
      label: 'At least one pilot placed in a class',
      met: memberCount >= 1,
      detail: `${memberCount} placed`
    },
    { label: 'A timer chosen for the event', met: timerCount >= 1, detail: `${timerCount} chosen` },
    { label: 'At least one round defined', met: roundCount >= 1, detail: `${roundCount} defined` }
  ]);
  const ready = $derived(checks.every((c) => c.met));
</script>

<!-- svelte:window must be top-level; the handler no-ops while the wizard is closed. -->
<svelte:window onkeydown={onKeydown} />

{#if open}
  <!-- A full-screen overlay so the embedded stage components have room to breathe (the shared
       Dialog is sized for small forms). Esc + the backdrop click both close. -->
  <div
    class="wizard-overlay"
    role="dialog"
    aria-modal="true"
    aria-label="Event setup wizard"
    tabindex="-1"
  >
    <button type="button" class="overlay-backdrop" aria-label="Close setup wizard" onclick={finish}
    ></button>

    <div class="wizard-panel">
      <header class="wizard-head">
        <div class="head-text">
          <p class="eyebrow">Set up · {ev?.name ?? 'this event'}</p>
          <h2 class="step-title">{current.label}</h2>
          <p class="step-blurb">{current.blurb}</p>
        </div>
        <button type="button" class="wizard-close" onclick={finish} aria-label="Close">×</button>
      </header>

      <!-- The progress indicator: a clickable step per stage. -->
      <ol class="steps" aria-label="Setup steps">
        {#each STEPS as s, i (s.id)}
          <li class:active={i === step} class:done={i < step}>
            <button
              type="button"
              onclick={() => (step = i)}
              aria-current={i === step ? 'step' : undefined}
            >
              <span class="step-num">{i + 1}</span>
              <span class="step-name">{s.label}</span>
            </button>
          </li>
        {/each}
      </ol>

      <div class="wizard-body">
        {#if current.id === 'classes'}
          <EventClasses {session} />
        {:else if current.id === 'roster'}
          <EventRoster {session} />
        {:else if current.id === 'timers'}
          <EventTimers {session} />
        {:else if current.id === 'rounds'}
          <EventRounds bind:this={roundsStage} {session} />
        {:else}
          <section class="review" aria-label="Ready to race?">
            <h3 class="review-title">Ready to race?</h3>
            <p class="review-sub">
              Everything below stays editable on the event's pages — this is just a quick check. You
              can finish and adjust anything later.
            </p>
            <ul class="review-checks">
              {#each checks as c (c.label)}
                <li class="review-check" class:met={c.met}>
                  <span class="check-icon" aria-hidden="true">{c.met ? '✓' : '○'}</span>
                  <span class="check-label">{c.label}</span>
                  <span class="check-detail">{c.detail}</span>
                </li>
              {/each}
            </ul>
            <p class="review-verdict" class:ready aria-live="polite">
              {ready
                ? 'This event is ready to race.'
                : 'You can still finish — the unmet items are optional and editable later.'}
            </p>
          </section>
        {/if}
      </div>

      <footer class="wizard-foot">
        <div class="foot-left">
          <Button variant="ghost" onclick={back} disabled={isFirst}>Back</Button>
        </div>
        <div class="foot-right">
          {#if !isLast}
            <Button variant="ghost" onclick={skip}>Skip</Button>
            <Button variant="primary" onclick={next}>Next</Button>
          {:else}
            <Button variant="primary" onclick={finish}>Finish setup</Button>
          {/if}
        </div>
      </footer>
    </div>
  </div>
{/if}

<style>
  .wizard-overlay {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: grid;
    place-items: center;
    padding: var(--gf-space-5);
    font-family: var(--gf-font-family);
    color: var(--gf-text);
  }
  .overlay-backdrop {
    position: absolute;
    inset: 0;
    border: none;
    padding: 0;
    margin: 0;
    background: rgba(4, 7, 11, 0.62);
    backdrop-filter: blur(3px);
    cursor: pointer;
  }
  .wizard-panel {
    position: relative;
    z-index: 1;
    display: flex;
    flex-direction: column;
    width: min(56rem, 100%);
    max-height: 92vh;
    background: var(--gf-overlay, var(--gf-surface));
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-lg);
    box-shadow: var(--gf-shadow-lg);
    overflow: hidden;
  }
  .wizard-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--gf-space-4);
    padding: var(--gf-space-5) var(--gf-space-6) var(--gf-space-4);
    border-bottom: 1px solid var(--gf-border-subtle);
  }
  .head-text {
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-1);
    min-width: 0;
  }
  .eyebrow {
    margin: 0;
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
    color: var(--gf-accent);
  }
  .step-title {
    margin: 0;
    font-size: var(--gf-font-size-xl);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-tight);
  }
  .step-blurb {
    margin: 0;
    font-size: var(--gf-font-size-md);
    color: var(--gf-text-secondary);
    line-height: 1.5;
  }
  .wizard-close {
    flex-shrink: 0;
    border: none;
    background: transparent;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-2xl);
    line-height: 1;
    cursor: pointer;
    border-radius: var(--gf-radius-xs);
  }
  .wizard-close:hover {
    color: var(--gf-text);
  }
  .wizard-close:focus-visible {
    outline: none;
    box-shadow: var(--gf-focus-ring);
  }

  .steps {
    display: flex;
    flex-wrap: wrap;
    gap: var(--gf-space-2);
    list-style: none;
    margin: 0;
    padding: var(--gf-space-3) var(--gf-space-6);
    border-bottom: 1px solid var(--gf-border-subtle);
    background: var(--gf-surface-alt);
  }
  .steps li button {
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    background: transparent;
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-pill);
    padding: var(--gf-space-2) var(--gf-space-3);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
    cursor: pointer;
    transition:
      color var(--gf-motion-fast) var(--gf-ease-out),
      background var(--gf-motion-fast) var(--gf-ease-out),
      border-color var(--gf-motion-fast) var(--gf-ease-out);
  }
  .steps li button:hover {
    color: var(--gf-text);
    border-color: var(--gf-border);
  }
  .step-num {
    display: inline-grid;
    place-items: center;
    width: 1.4rem;
    height: 1.4rem;
    border-radius: 50%;
    background: var(--gf-surface-sunken);
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-semibold);
    font-variant-numeric: tabular-nums;
  }
  .steps li.active button {
    border-color: var(--gf-accent);
    background: var(--gf-accent-soft);
    color: var(--gf-accent);
    font-weight: var(--gf-font-weight-semibold);
  }
  .steps li.active .step-num {
    background: var(--gf-accent);
    color: var(--gf-accent-contrast, #000);
  }
  .steps li.done button {
    color: var(--gf-text-secondary);
    border-color: var(--gf-border);
  }
  .steps li.done .step-num {
    background: var(--gf-success-soft, var(--gf-surface-sunken));
    color: var(--gf-success, var(--gf-text-secondary));
  }

  .wizard-body {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: var(--gf-space-6);
  }

  .review {
    max-width: 40rem;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-3);
  }
  .review-title {
    margin: 0;
    font-size: var(--gf-font-size-lg);
    font-weight: var(--gf-font-weight-semibold);
  }
  .review-sub {
    margin: 0;
    font-size: var(--gf-font-size-md);
    color: var(--gf-text-secondary);
    line-height: 1.5;
  }
  .review-checks {
    list-style: none;
    margin: var(--gf-space-2) 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--gf-space-2);
  }
  .review-check {
    display: flex;
    align-items: center;
    gap: var(--gf-space-3);
    padding: var(--gf-space-3) var(--gf-space-4);
    border: 1px solid var(--gf-border-subtle);
    border-radius: var(--gf-radius-md);
    background: var(--gf-surface);
  }
  .review-check.met {
    border-color: color-mix(in srgb, var(--gf-success) 45%, var(--gf-border));
  }
  .check-icon {
    display: inline-grid;
    place-items: center;
    width: 1.6rem;
    height: 1.6rem;
    flex-shrink: 0;
    border-radius: 50%;
    background: var(--gf-surface-sunken);
    color: var(--gf-text-muted);
    font-weight: var(--gf-font-weight-bold);
  }
  .review-check.met .check-icon {
    background: var(--gf-success-soft, var(--gf-surface-sunken));
    color: var(--gf-success, var(--gf-text));
  }
  .check-label {
    flex: 1;
    min-width: 0;
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-medium);
  }
  .check-detail {
    flex-shrink: 0;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
    font-variant-numeric: tabular-nums;
  }
  .review-verdict {
    margin: var(--gf-space-2) 0 0;
    font-size: var(--gf-font-size-md);
    font-weight: var(--gf-font-weight-medium);
    color: var(--gf-text-secondary);
  }
  .review-verdict.ready {
    color: var(--gf-success, var(--gf-text));
  }

  .wizard-foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-3);
    padding: var(--gf-space-4) var(--gf-space-6);
    border-top: 1px solid var(--gf-border-subtle);
    background: var(--gf-surface-alt);
  }
  .foot-right {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
  }
</style>
