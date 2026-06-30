/**
 * Real MultiGP multi-main contract (#323): composes + runs a full 64-pilot multi-main **end to
 * end** against the real Director, proving the two new seeding rules (`FromRankingRange`,
 * `Combine`) and per-main **bracket composition** work the way a real MultiGP event wires them.
 *
 * Unlike the single `multi_main` format generator (see structures.contract.ts), a real MultiGP
 * multi-main is *composed by the RD out of ordinary rounds*: a flat qualifier, then a stack of
 * per-main single-elimination brackets (C → B → A), each seeded from a **slice** of the qual
 * ranking (`FromRankingRange`) optionally **combined** with the **bump-ups** from the main below
 * (`Combine` of a range + the lower main's final top-2). The A main's final is a `chase_the_ace`.
 *
 * The composition under test (every heat is **exactly 4 pilots**):
 *
 *   qual      round_robin, 1 round, heat_size 4   → 16 heats of 4, a 64-pilot ranking
 *   c-l1      single_elim (4-up, advance 2)        FromRankingRange(qual, skip 12, take 8)  → 2 heats
 *   c-final   single_elim (4-up, advance 2)        FromHeatWinners(c-l1)                    → 1 heat
 *   b-l1      single_elim (4-up, advance 2)        Combine[ Range(qual, 6, 6), Ranking(c-final, 2) ] → 2 heats
 *   b-final   single_elim (4-up, advance 2)        FromHeatWinners(b-l1)                    → 1 heat
 *   a-l1      single_elim (4-up, advance 2)        Combine[ Range(qual, 0, 6), Ranking(b-final, 2) ] → 2 heats
 *   a-final   chase_the_ace (wins_to_win 2)        FromHeatWinners(a-l1)                    → ≥1 heat
 *
 * The slices `skip 0..6 / 6..12 / 12..20` are disjoint windows of the **same** qual ranking, so the
 * per-main fields never collide and every Combine de-dupes to a clean 8 → two heats of four. Each
 * lower main's final top-2 **bumps up** into the next main's level-1 field; the test asserts those
 * exact bump refs land in the next main's lineups, that *every* scheduled heat across *every* round
 * is size 4, and that the A main crowns a champion.
 *
 * The sim makes the front of each heat's lineup fastest (a deterministic per-seat pace), so every
 * heat's winners and thus the whole bracket carry are deterministic. NB: the spec sketched the qual
 * as `timed_qual`, but `timed_qual` flies the *whole field in one heat* (it ignores `heat_size`) — it
 * cannot produce 16 heats of 4. `round_robin` with one round + `heat_size: 4` is the format that
 * partitions a flat field into heats of four while still ranking all 64, so the qual uses that.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import {
  createClass,
  createPilot,
  createRound,
  PRACTICE_EVENT_ID,
  roundRanking,
  setClassMembership,
  setEventClasses,
  setEventRoster
} from '../packages/protocol-client/dist/index.js';
import { type Director } from '../test-harness/director.ts';
import { driveToRunning, eventRoot, rdControl, startContractDirector } from './harness.ts';

const TOKEN = 'rd-multi-main-real-contract';

let director: Director;

beforeAll(async () => {
  // A brisk sim (one quick lap) so each of the ~26 heats scores fast; the sim makes earlier seats
  // fastest, so every heat's winners — and thus the bracket carry — are deterministic.
  director = await startContractDirector({ token: TOKEN, simLaps: 1, simLapMs: 25 });
}, 60_000);

afterAll(async () => {
  await director?.stop();
});

/** Every heat tagged with `round`, in schedule order, as `{ id, lineup }`. */
async function heatsOfRound(roundId: string): Promise<Array<{ id: string; lineup: string[] }>> {
  const resp = await fetch(`${eventRoot(director.baseUrl)}/heats`);
  const heats = (await resp.json()) as Array<{ heat: string; round?: string; lineup: string[] }>;
  return heats
    .filter((h) => h.round === roundId)
    .map((h) => ({ id: h.heat, lineup: h.lineup.map(String) }));
}

/** FillRound (All) a round, surfacing the ack error if it is rejected. */
async function fillAll(roundId: string): Promise<void> {
  const ack = await rdControl(director.baseUrl, TOKEN, {
    FillRound: { round: roundId, mode: 'All' }
  });
  if (!ack.ok) throw new Error(`FillRound ${roundId} rejected: ${JSON.stringify(ack.error)}`);
}

/** Drive a heat to Running, wait until each competitor banks a lap, then ForceEnd + Finalize. */
async function runAndFinalize(heat: string, competitors: number): Promise<void> {
  await driveToRunning(director.baseUrl, TOKEN, heat);
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    const resp = await fetch(
      `${eventRoot(director.baseUrl)}/snapshot/heat/${heat}?projection=laps`
    );
    if (resp.ok) {
      const snap = (await resp.json()) as {
        body: { LapList?: { competitors: Array<{ laps: unknown[] }> } };
      };
      const cs = snap.body.LapList?.competitors ?? [];
      if (cs.length >= competitors && cs.every((c) => c.laps.length >= 1)) break;
    }
    await new Promise((r) => setTimeout(r, 25));
  }
  expect((await rdControl(director.baseUrl, TOKEN, { ForceEnd: { heat } })).ok).toBe(true);
  expect((await rdControl(director.baseUrl, TOKEN, { Finalize: { heat } })).ok).toBe(true);
}

/**
 * Drive a multi-wave round to completion: fill, run every freshly-scheduled heat, repeat until a
 * fill schedules nothing new (the generator returned Complete). Returns the heats in run order.
 */
async function drainRound(roundId: string): Promise<Array<{ id: string; lineup: string[] }>> {
  const run = new Set<string>();
  const order: Array<{ id: string; lineup: string[] }> = [];
  for (let guard = 0; guard < 64; guard++) {
    await fillAll(roundId);
    const fresh = (await heatsOfRound(roundId)).filter((h) => !run.has(h.id));
    if (fresh.length === 0) return order;
    for (const h of fresh) {
      await runAndFinalize(h.id, h.lineup.length);
      run.add(h.id);
      order.push(h);
    }
  }
  throw new Error(`drainRound(${roundId}) did not converge — generator never returned Complete`);
}

/** Assert every heat of `roundId` is exactly 4 pilots (and, if given, that there are `count`). */
async function assertHeatsAllSizeFour(
  roundId: string,
  count?: number
): Promise<Array<{ id: string; lineup: string[] }>> {
  const heats = await heatsOfRound(roundId);
  if (count !== undefined) expect(heats.length, `round ${roundId} heat count`).toBe(count);
  for (const h of heats) {
    expect(h.lineup.length, `heat ${h.id} (round ${roundId}) lineup size`).toBe(4);
  }
  return heats;
}

/** A round's ranking as plain competitor-ref strings, best first. */
async function ranking(roundId: string): Promise<string[]> {
  const rank = await roundRanking(director.baseUrl, PRACTICE_EVENT_ID, roundId);
  return rank.map((e) => String(e.competitor));
}

/** Create a 4-up single-elim level (advance 2, First-to-1-lap) with the given seeding. */
function createLevel(label: string, klassId: string, seeding: unknown): Promise<{ id: string }> {
  return createRound(
    director.baseUrl,
    PRACTICE_EVENT_ID,
    {
      label,
      classes: [klassId],
      format: 'single_elim',
      params: { heat_size: '4', advance: '2' },
      win_condition: { FirstToLaps: { n: 1 } },
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      seeding: seeding as any,
      channel_mode: 'PerHeat'
    },
    TOKEN
  ) as Promise<{ id: string }>;
}

describe('real MultiGP multi-main: composed seeding rules + per-main brackets, end to end', () => {
  it('seeds C/B/A mains from ranking slices + bump-ups and crowns a champion — all heats size 4', async () => {
    // 1) A class + 64 pilots (q01..q64), selected + rostered + membered on Practice.
    const klass = await createClass(director.baseUrl, { name: 'MultiGP 64' }, TOKEN);
    const ids: string[] = [];
    for (let i = 1; i <= 64; i++) {
      const cs = `q${String(i).padStart(2, '0')}`;
      const p = await createPilot(director.baseUrl, { callsign: cs }, TOKEN);
      ids.push(p.id);
    }
    await setEventClasses(director.baseUrl, PRACTICE_EVENT_ID, [klass.id], TOKEN);
    await setEventRoster(director.baseUrl, PRACTICE_EVENT_ID, ids, TOKEN);
    await setClassMembership(director.baseUrl, PRACTICE_EVENT_ID, klass.id, ids, TOKEN);

    // 2) Qualifying: one round_robin round of heats of four over the whole field, FromRoster. This
    //    partitions the 64 into 16 heats of 4 and ranks all 64 — the flat seed every main slices.
    const qual = await createRound(
      director.baseUrl,
      PRACTICE_EVENT_ID,
      {
        label: 'Qualifying',
        classes: [klass.id],
        format: 'round_robin',
        params: { rounds: '1', heat_size: '4' },
        win_condition: 'BestLap',
        time_limit_secs: 60,
        seeding: 'FromRoster',
        channel_mode: 'PerHeat'
      },
      TOKEN
    );
    await drainRound(qual.id);
    await assertHeatsAllSizeFour(qual.id, 16);
    const qualRank = await ranking(qual.id);
    expect(qualRank.length, 'qual ranks all 64 pilots').toBe(64);

    // The three disjoint seed windows the mains slice from the qual ranking.
    const aSeeds = qualRank.slice(0, 6); //  seeds 1..6   → A main base
    const bSeeds = qualRank.slice(6, 12); //  seeds 7..12  → B main base
    const cSeeds = qualRank.slice(12, 20); // seeds 13..20 → C main (8 → two heats)

    // 3) C main, level 1: the 8 seeds 13..20 (FromRankingRange skip 12 take 8) → two 4-up heats.
    const cL1 = await createLevel('C main — Level 1', klass.id, {
      FromRankingRange: { source_rounds: [qual.id], skip: 12, take: 8 }
    });
    await fillAll(cL1.id);
    const cL1Heats = await assertHeatsAllSizeFour(cL1.id, 2);
    expect(new Set(cL1Heats.flatMap((h) => h.lineup))).toEqual(new Set(cSeeds));
    for (const h of cL1Heats) await runAndFinalize(h.id, 4);

    // 4) C final: the C-L1 winners (FromHeatWinners) → one heat of 4. Two 4-up heats advancing two
    //    each means exactly **4 advance**, which is precisely a single full heat of 4 here — the
    //    FromHeatWinners carry must NOT also drag the 3rd-place losers up (the 4-up advancement bug).
    //    Its top 2 then bump up to B.
    const cFinal = await createLevel('C main — Final', klass.id, {
      FromHeatWinners: { source_round: cL1.id }
    });
    await drainRound(cFinal.id);
    const cFinalHeats = await assertHeatsAllSizeFour(cFinal.id, 1);
    expect(cFinalHeats[0].lineup.length, 'C-L1 advances exactly 4 (no 3rd-place losers)').toBe(4);
    const cBump = (await ranking(cFinal.id)).slice(0, 2);
    expect(cBump.length).toBe(2);

    // 5) B main, level 1: Combine[ qual seeds 7..12, C-final top 2 ] → 8 pilots → two heats of 4.
    //    Assert (pre-drain) the two C bumps AND the six base seeds are present in the lineups.
    const bL1 = await createLevel('B main — Level 1', klass.id, {
      Combine: {
        sources: [
          { FromRankingRange: { source_rounds: [qual.id], skip: 6, take: 6 } },
          { FromRanking: { source_rounds: [cFinal.id], top_n: 2 } }
        ]
      }
    });
    await fillAll(bL1.id);
    const bL1Heats = await assertHeatsAllSizeFour(bL1.id, 2);
    const bL1Field = new Set(bL1Heats.flatMap((h) => h.lineup));
    for (const ref of cBump) expect(bL1Field.has(ref), `C bump ${ref} in B-L1`).toBe(true);
    for (const ref of bSeeds) expect(bL1Field.has(ref), `B base seed ${ref} in B-L1`).toBe(true);
    expect(bL1Field).toEqual(new Set([...bSeeds, ...cBump]));
    for (const h of bL1Heats) await runAndFinalize(h.id, 4);

    // 6) B final: the 4 B-L1 winners → one heat of 4. Its top 2 bump up to A.
    const bFinal = await createLevel('B main — Final', klass.id, {
      FromHeatWinners: { source_round: bL1.id }
    });
    await drainRound(bFinal.id);
    await assertHeatsAllSizeFour(bFinal.id, 1);
    const bBump = (await ranking(bFinal.id)).slice(0, 2);
    expect(bBump.length).toBe(2);

    // 7) A main, level 1: Combine[ qual seeds 1..6, B-final top 2 ] → 8 pilots → two heats of 4.
    const aL1 = await createLevel('A main — Level 1', klass.id, {
      Combine: {
        sources: [
          { FromRankingRange: { source_rounds: [qual.id], skip: 0, take: 6 } },
          { FromRanking: { source_rounds: [bFinal.id], top_n: 2 } }
        ]
      }
    });
    await fillAll(aL1.id);
    const aL1Heats = await assertHeatsAllSizeFour(aL1.id, 2);
    const aL1Field = new Set(aL1Heats.flatMap((h) => h.lineup));
    for (const ref of bBump) expect(aL1Field.has(ref), `B bump ${ref} in A-L1`).toBe(true);
    for (const ref of aSeeds) expect(aL1Field.has(ref), `A base seed ${ref} in A-L1`).toBe(true);
    expect(aL1Field).toEqual(new Set([...aSeeds, ...bBump]));
    for (const h of aL1Heats) await runAndFinalize(h.id, 4);

    // 8) A final: chase_the_ace over the 4 A-L1 winners, first to 2 race-wins. Multi-wave: drain
    //    until a finalist reaches 2 wins. Every race is a heat of 4; a champion is crowned.
    const aFinal = await createRound(
      director.baseUrl,
      PRACTICE_EVENT_ID,
      {
        label: 'A main — Final (Chase the Ace)',
        classes: [klass.id],
        format: 'chase_the_ace',
        params: { wins_to_win: '2' },
        win_condition: { FirstToLaps: { n: 1 } },
        seeding: { FromHeatWinners: { source_round: aL1.id } },
        channel_mode: 'PerHeat'
      },
      TOKEN
    );
    const aFinalHeats = await drainRound(aFinal.id);
    expect(aFinalHeats.length, 'chase runs at least one race').toBeGreaterThanOrEqual(1);
    await assertHeatsAllSizeFour(aFinal.id);
    const champ = await roundRanking(director.baseUrl, PRACTICE_EVENT_ID, aFinal.id);
    expect(champ.length).toBeGreaterThanOrEqual(1);
    expect(String(champ[0].competitor), 'a champion is crowned').toBeTruthy();
    expect(champ[0].position).toBe(1);
  }, 120_000);
});
