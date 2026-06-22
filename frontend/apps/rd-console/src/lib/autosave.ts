/**
 * autosave — a tiny per-target debounced saver for the event setup stages.
 *
 * The selection stages (Classes / Pilots / Channels / Timers) used to persist behind an explicit
 * "Save" button; they now persist **on every change**. Because each of these saves is a
 * *wholesale-set-the-full-selection* call (`setEventClasses` / `setEventRoster` /
 * `setClassMembership` / `setEventTimers` send the entire current selection, not a delta), saving
 * the current selection on each toggle is last-write-wins with no partial/race hazard.
 *
 * This helper gives each logical **target** (e.g. "classes", "roster", a class id for membership)
 * its own debounce timer so rapid clicking on one target coalesces into a single trailing save,
 * while different targets save independently. The UI flips **optimistically** (the caller mutates
 * its working state synchronously before scheduling the save); on a save **error** the caller's
 * `onError` runs so it can revert that change and surface the error affordance.
 */

/** The default coalescing window — rapid clicks within this gap fold into one trailing save. */
export const AUTOSAVE_DEBOUNCE_MS = 300;

/** A scheduled save for one target: it computes the payload lazily (at flush time) and persists it. */
export interface SaveSpec<T> {
  /**
   * Produce the value to persist, evaluated when the debounce fires (so it captures the *latest*
   * working selection, not a stale snapshot from when the save was first scheduled).
   */
  readonly compute: () => T;
  /** Persist the computed value. Resolves to the server result (falsy → treated as "not saved"). */
  readonly save: (value: T) => Promise<unknown>;
  /**
   * Called when {@link save} resolves falsy (e.g. a control token was declined). The change was
   * applied optimistically; the caller may leave it (it'll be re-saved on the next change) or note
   * it. Distinct from {@link onError} (a thrown failure).
   */
  readonly onUnsaved?: () => void;
  /** Called when {@link save} throws — the caller reverts the optimistic change and surfaces it. */
  readonly onError?: (error: unknown) => void;
}

/**
 * A debounced auto-saver keyed by target. Call {@link schedule} after applying an optimistic
 * change; the matching target's trailing save fires once the {@link AUTOSAVE_DEBOUNCE_MS} window
 * goes quiet. {@link flush} forces any pending save immediately; {@link cancel} drops it.
 */
export class AutoSaver {
  readonly #timers = new Map<string, ReturnType<typeof setTimeout>>();
  readonly #pending = new Map<string, SaveSpec<unknown>>();
  readonly #delay: number;

  constructor(delay = AUTOSAVE_DEBOUNCE_MS) {
    this.#delay = delay;
  }

  /** Whether a save is currently queued for `target` (debouncing or awaiting flush). */
  isPending(target: string): boolean {
    return this.#timers.has(target);
  }

  /** Schedule (or re-schedule) a trailing save for `target`, coalescing rapid changes. */
  schedule<T>(target: string, spec: SaveSpec<T>): void {
    this.#pending.set(target, spec as SaveSpec<unknown>);
    const existing = this.#timers.get(target);
    if (existing !== undefined) clearTimeout(existing);
    this.#timers.set(
      target,
      setTimeout(() => void this.#run(target), this.#delay)
    );
  }

  /** Force the pending save for `target` (if any) to run now. */
  flush(target: string): void {
    const existing = this.#timers.get(target);
    if (existing !== undefined) {
      clearTimeout(existing);
      void this.#run(target);
    }
  }

  /** Drop any pending save for `target` without running it. */
  cancel(target: string): void {
    const existing = this.#timers.get(target);
    if (existing !== undefined) clearTimeout(existing);
    this.#timers.delete(target);
    this.#pending.delete(target);
  }

  /** Drop every pending save (e.g. on teardown). */
  cancelAll(): void {
    for (const t of this.#timers.values()) clearTimeout(t);
    this.#timers.clear();
    this.#pending.clear();
  }

  async #run(target: string): Promise<void> {
    this.#timers.delete(target);
    const spec = this.#pending.get(target);
    this.#pending.delete(target);
    if (!spec) return;
    try {
      const value = spec.compute();
      const result = await spec.save(value);
      if (!result) spec.onUnsaved?.();
    } catch (e) {
      spec.onError?.(e);
    }
  }
}
