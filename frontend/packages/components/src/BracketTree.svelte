<script lang="ts">
  import type { Bracket } from './bracket.js';

  /**
   * BracketTree — a single-elimination bracket view (rounds → heats → winners).
   *
   * Source type: the local `Bracket` view-model (see `bracket.ts` for why this
   * is a component-local type rather than a wire type, and how a caller derives
   * it from heats + `HeatResult`s). Rounds render left-to-right, matches stacked
   * within each round; the advancing seat in each match is marked.
   */
  let { bracket }: { bracket: Bracket } = $props();
</script>

<div class="gridfpv-bracket" role="tree" aria-label="Single-elimination bracket">
  {#each bracket.rounds as round (round.name)}
    <section class="round" role="group" aria-label={round.name}>
      <h4 class="round-name">{round.name}</h4>
      {#each round.matches as match, mi (match.heat ?? round.name + '-' + mi)}
        <div class="match" role="group" aria-label={match.heat ?? `Match ${mi + 1}`}>
          {#each match.slots as slot, si (si)}
            <div
              class="slot"
              class:winner={slot.winner}
              role="treeitem"
              aria-selected={slot.winner ?? false}
            >
              <span class="competitor">{slot.label ?? slot.competitor ?? 'TBD'}</span>
              {#if slot.winner}<span class="check" aria-hidden="true">✓</span>{/if}
            </div>
          {/each}
        </div>
      {/each}
    </section>
  {/each}
</div>

<style>
  .gridfpv-bracket {
    display: flex;
    gap: var(--gf-space-6);
    align-items: stretch;
    color: var(--gf-text);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-sm);
    overflow-x: auto;
  }
  .round {
    display: flex;
    flex-direction: column;
    justify-content: space-around;
    gap: var(--gf-space-4);
    min-width: 10rem;
  }
  .round-name {
    margin: 0;
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-2xs);
    font-weight: var(--gf-font-weight-semibold);
    text-transform: uppercase;
    letter-spacing: var(--gf-tracking-caps);
  }
  .match {
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-md);
    overflow: hidden;
    background: var(--gf-elevated);
    box-shadow: var(--gf-shadow-xs);
  }
  .slot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gf-space-2);
    padding: var(--gf-space-3) var(--gf-space-3);
    transition: background var(--gf-motion-fast) var(--gf-ease-out);
  }
  .slot + .slot {
    border-top: 1px solid var(--gf-border-subtle);
  }
  .slot.winner {
    background: var(--gf-accent-soft);
    font-weight: var(--gf-font-weight-bold);
    box-shadow: inset 2px 0 0 var(--gf-accent);
  }
  .slot.winner .competitor {
    color: var(--gf-accent);
  }
  .check {
    color: var(--gf-accent);
    font-weight: var(--gf-font-weight-bold);
  }
</style>
