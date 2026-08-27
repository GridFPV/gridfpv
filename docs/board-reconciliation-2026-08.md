# Board reconciliation — August 2026

**Status:** open loose ends, not yet actioned. Written 2026-08-23 on returning to the
project after ~7 weeks away (the gap between cutting `v0.4.0-alpha.1` on 2026-07-05 and
picking it back up).

This is a working ledger, not a design doc. It records what is stale on the GitHub board
and how to settle it. Delete it once the reconciliation pass is done.

---

## Why this exists

`v0.4.0-alpha.1` was cut on 2026-07-05 and **never tested** — it was built specifically to
try GridFPV against a physical RotorHazard timer, and that test never happened before the
move. Release asset download counts confirm it: Linux 0, macOS 0, Windows 1.

The board was last touched around that cut. Since then the only commits are the dev-docs
reconciliation (`d1ab5cb` + merge `4e0f796`, PR #379) — **zero code changes**. So the board
reflects the state of a codebase that hasn't moved, and any drift in it is drift that
already existed at the alpha.

Repo state at time of writing:

| | |
|---|---|
| Tag `v0.4.0-alpha.1` | `b8b9054` |
| `origin/main` | level with the tag (0 commits ahead) |
| `origin/devel` | 2 commits ahead — both docs-only |
| Open PRs | none |
| Open issues | 68 |

---

## Loose end 1 — `gh` cannot read Projects v2 — **RESOLVED, see Correction below**

`gh project list --owner GridFPV` fails:

```
GraphQL: Resource not accessible by personal access token (organization.projectsV2.nodes.0)
```

The `gh` login uses a **fine-grained PAT** (`github_pat_…`) that lacks the project scope.
Everything else — issues, releases, milestones, labels, PRs, the API — works fine. This
only blocks the project *board* (columns, card positions, custom fields).

**Fix:** add the scope at <https://github.com/settings/personal-access-tokens>, then
re-authenticate. Note `gh auth refresh -s read:project` does **not** work for a fine-grained
PAT — the scope has to be added to the token itself, or swap to an OAuth web login.

- `read:project` — enough for me to read and report on the board.
- `project` — needed for me to move cards / edit fields.

Until then, reconciliation can still be done entirely through issues, labels, and
milestones; only the board columns stay out of reach.

---

## Loose end 2 — 27 issues sitting in `status/review` — **PREMISE WRONG, see Correction below**

`status/review` maps to the board's **Review** column. 27 open issues carry it, and the bulk
of them look like work that shipped in the alpha and was simply never closed out. They need
a pass against the actual code, one at a time.

| # | Milestone | Area | Title |
|---|---|---|---|
| 39 | none | engine | Unify marshaling-fold: `score_marshaled` vs `lap_list_marshaled` (v0.3 follow-up) |
| 40 | v0.4 | protocol | Protocol server: `gridfpv-server` crate + wire-type module [BE] |
| 41 | v0.4 | protocol | Protocol: live race-state projection [BE] |
| 42 | v0.4 | protocol | Protocol: snapshot HTTP endpoints + scope grammar [BE] |
| 43 | v0.4 | protocol | Protocol: WS change-stream + sequence/resume engine [BE] |
| 44 | v0.4 | protocol | Protocol: auth & authorization [BE] |
| 45 | v0.4 | protocol | Protocol: RD control / write path [BE] |
| 46 | v0.4 | protocol | Protocol: contract version negotiation + error model [BE] |
| 47 | v0.4 | protocol | Protocol: mock-RH end-to-end server test [BE] ⭐ |
| 48 | v0.4 | clients | Frontend: monorepo + Svelte/Vite scaffold [FE] |
| 49 | v0.4 | clients | Frontend: generated-types wiring + protocol client [FE] |
| 50 | v0.4 | clients | Frontend: shared component library v1 [FE] |
| 51 | v0.4 | clients | RD console: shell + RD auth + layout [FE] |
| 52 | v0.4 | clients | RD console: setup wizard [FE] |
| 53 | v0.4 | clients | RD console: registration [FE] |
| 54 | v0.4 | clients | RD console: live race control [FE] |
| 55 | v0.4 | clients | RD console: marshaling UI [FE] |
| 56 | v0.4 | clients | RD console: results [FE] |
| 59 | v0.4 | docs | v0.4 doc-reconciliation pass |
| 60 | v0.4 | clients, protocol | Registration binding: `CompetitorRegistered` event + projection pilot-mapping |
| 62 | v0.4 | clients, protocol | Protocol: authoritative race-start clock in `LiveRaceState` |
| 63 | v0.4 | protocol | Protocol: HTTP endpoint to mint a read-only join token (QR/URL LAN access) |
| 64 | v0.4 | protocol | Protocol: unknown API routes masked by the SPA fallback |
| 67 | v0.4 | engine | Scoring: apply penalties (DQ / time-added) + heat-void |
| 68 | v0.4 | engine | Format: double-elimination generator |
| 69 | v0.4 | engine | Format: round-robin generator |
| 70 | v0.4 | engine | Format: multi-tier mains (A/B/C/D/E) |

**Caution on the `[FE]`/`[BE]` slice issues (#40–#56).** These read as scaffolding tickets
whose *headline* shipped, but several have acceptance criteria broader than what landed.
Closing them on title alone would bury real remaining work. Each needs its body read against
the code before a verdict.

---

## Loose end 3 — 2 `status/blocked` issues that demonstrably shipped — **see Correction below**

| # | Title |
|---|---|
| 57 | Tauri shell wrapping the RD console [PKG] |
| 58 | Single-binary packaging Win/Linux/macOS [PKG] |

Both are still labelled **blocked**, but `v0.4.0-alpha.1` ships self-contained Tauri
portables for all three platforms, and the docs-reconciliation commit explicitly records
"protocol: Tauri packaging SHIPPED". `src-tauri/` exists and `.github/workflows/release-builds.yml`
builds the matrix.

These are the highest-confidence closes on the board. Verify the acceptance criteria in each
body, then close.

---

## Loose end 4 — milestone and tracking hygiene

- **18 open issues carry no milestone**, including several explicitly scoped as post-v1
  (#353, #355, #357, #358) and one flagged ⭐ TOP PRIORITY post-v1 (#355). There is no
  post-v1 / v1.x milestone for them to live in — worth creating one so they stop floating.
- **5 tracking issues** (#13–#17), one per milestone v0.4–v0.8. #13 is titled
  "v0.4 — Director server + RD console" while the milestone itself reads "v0.4 — Direct a
  single race (RD console, vertical slices)". Reconcile the wording.
- **No milestone has a due date.** Fine if deliberate; noting it so it's a choice.

Milestone counts as of writing:

| Milestone | Open | Closed |
|---|---|---|
| v0.4 — Direct a single race | 41 | 12 |
| v0.5 — Racer/spectator PWA | 3 | 0 |
| v0.6 — Streaming / broadcast | 1 | 0 |
| v0.7 — Cloud | 3 | 0 |
| v0.8 — Integrations | 2 | 0 |

---

## Already verified this session

Settled with the restored toolchain, so they don't need re-litigating in the pass:

- **#251 is stale — close it.** It claims the live-gated `gridfpv-server` `full_event_live`
  test "doesn't compile on devel (router/EventRegistry signature drift)". It compiles clean
  today: `cargo check -p gridfpv-server --features live --all-targets` → exit 0. The
  signature drift was fixed at some point without the issue being updated. *Caveat:*
  compiling is not passing — the test is `#[ignore]`d and needs docker to actually run, so
  confirm it green under `cargo test -p gridfpv-server --features live --test full_event_live -- --ignored`
  before closing on the stronger claim.
- **#59 (v0.4 doc-reconciliation pass) is done.** Commit `d1ab5cb` / PR #379 is exactly this
  work — four parallel audits against shipped code, applied across roadmap, vision,
  timer-adapters, rotorhazard-plugin, and protocol docs.
- **Workspace is healthy.** `cargo check --workspace --all-targets` → exit 0 in 1m55s. Only
  warnings are pre-existing `ts-rs` serde-attribute parse notes.

---

## Method for the reconciliation pass

For each `status/review` issue, in ascending number order:

1. Read the issue body — the **acceptance criteria**, not the title.
2. Locate the implementing code and any regression test covering it.
3. Verdict, one of:
   - **close** — criteria met, test exists, cite the commit/PR and test path in the closing comment;
   - **partial** — headline shipped but criteria remain; strip `status/review`, edit the body
     down to the residual work, keep it open;
   - **stale** — the criteria no longer describe what the project wants; close with a note
     saying why, or rewrite.
4. Anything that turns out to be genuinely unfinished keeps its milestone and loses
   `status/review` so the Review column stops lying.

Sequencing note: do #57/#58 first (highest confidence), then the engine issues (#67–#70,
verifiable against `crates/engine` tests), then protocol (#40–#47), then the frontend slices
(#48–#56, the murkiest — RD console criteria are the most likely to have residual work).

Cross-check everything against `docs/release-audit-2026-07.html`, which is the ledger of the
pre-release audit, and `docs/roadmap.html`, which the reconciliation commit annotated
done/cut per slice.

---

## Environment restored (2026-08-23)

Context for anyone picking this up — the dev box was rebuilt from scratch after the move.

| Tool | Version | Location |
|---|---|---|
| `gh` | 2.98.0 | `~/.local/bin` (authed as `ryan-johnson2`, SSH protocol) |
| Rust + cargo/rustfmt/clippy | 1.98.0 | `~/.cargo` (sourced from `~/.bashrc`) |
| Node / npm | 22.23.2 / 10.9.8 | `~/.local/lib/nodejs`, symlinked into `~/.local/bin` |
| npm deps | installed | `frontend/` (310 pkgs) and the `docs` repo |
| System build tools | installed | `build-essential`, `pkg-config`, `libssl-dev` |
| Docker | **pending** | needed for `cargo xtask rh-mock` / `cargo xtask live` |

**This box is headless** (`XDG_SESSION_TYPE=tty`, no `DISPLAY`) and has no webkit2gtk/GTK
runtime, so the Tauri desktop portable cannot run here. That is deliberate: it stays a
dev/build box, and physical-RH testing happens on a laptop using the prebuilt portable from
the release. `tauri-cli` and the webkit `-dev` packages are therefore **not** installed and
are not needed unless that decision changes.

`npm audit` reports vulnerabilities in both dependency trees (docs: 5 — 2 moderate, 3 high).
Dev tooling only, not shipped code. Worth a look, doesn't gate testing.

---

# Correction — board access restored (2026-08-23, later same day)

Docker was installed and the `gh` token was swapped to OAuth. Both unblocked loose ends,
and seeing the actual board **invalidates the central assumption of Loose end 2**.

## Loose end 1 — resolved

Swapped the fine-grained PAT for an OAuth login (`gho_…`), scopes
`gist, project, read:org, repo`. `gh project list --owner GridFPV` now works:

| | |
|---|---|
| Project | #1 **GridFPV Roadmap** (`PVT_kwDOEZrkZc4BbONH`) |

Root cause worth remembering: the old token was fine-grained, and fine-grained PATs reach
org-owned Projects only when the token's **resource owner is the org itself** — a separate
setting from its permissions. `gh auth refresh -s read:project` can never fix this, because
`refresh` manipulates OAuth scopes, which a fine-grained PAT does not have. The old PAT also
expired 2026-09-18; OAuth removes that clock. (Most `gh` commands appeared to "work fine"
under it only because **this repo is public** — that was near-unauthenticated read, not
token permission.)

## Loose end 2 — the premise was wrong

The ledger assumed `status/review` maps to a board **Review** column and that the column is
"lying". Neither holds:

- **There is no Review column.** The board's `Status` field has exactly three options:
  **Todo, In Progress, Done**. No Review, no Blocked.
- **The board holds 13 items, not 68.** And the 27 open `status/review` issues are
  **not among them** — the two sets are strictly disjoint:

| set | members |
|---|---|
| open `status/review` | #39–#56, #59, #60, #62, #63, #64, #67–#70 (27) |
| on the board | #71–#78, #85, #90, #91, #112, #118 (13) |

So the board says **nothing at all** about the 27 issues. It is not stale — it is
*unpopulated*: **55 of 68 open issues have never been added to it.**

What the board actually tracks, and it is internally consistent:

| Status | count | what they are |
|---|---|---|
| Done | 9 | all **CLOSED** (#71–#74, #85, #90, #91, #112, #118) |
| Todo | 4 | all **OPEN** — #75–#78, Slices 4–7 |

The real label defect is smaller and cosmetic: **9 closed issues still carry
`status/review`**. Closing them never stripped the label.

**Revised action.** The reconciliation pass is a pass over *labels and issues*, not over
board columns — the board needs *populating* (or a decision that it only ever tracks the
Slice epics), which is a different question from whether #39–#70 are done. Settle these
separately:

1. Strip `status/review` from the 9 closed issues — pure hygiene, no code reading needed.
2. Audit the 27 open `status/review` issues against the code (the method in the original
   ledger still stands — it just has nothing to do with the board).
3. Decide the board's scope: mirror all open issues, or keep it an epic-level Slice board.
   Right now it is neither, which is why it looked stale.

## Loose end 3 — do NOT close #57/#58 yet

The ledger calls these "the highest-confidence closes on the board". They are not on the
board at all, and more importantly **their acceptance criteria are runtime claims**:

- #57 — "console loads in the Tauri window and talks to the local server"
- #58 — "the single binary launches on all three OSes"

CI builds the matrix and the portables exist, but per this same ledger the alpha was
**never launched anywhere** (downloads: Linux 0, macOS 0, Windows 1). Nobody has observed
either criterion being met, and this box is headless with no webkit so it cannot be observed
here. These are **blocked on the physical-laptop test**, which is exactly what
`status/blocked` already says. Leave them open.

## Live suite — Docker unblocked it

`cargo xtask live` runs. Two findings:

- **A live-gated test did not compile.** `crates/engine/tests/marshaling_live.rs` never got
  updated when `Event::LapInserted` gained its `heat: Option<HeatId>` field. Fixed by passing
  `heat: None` (the pre-tag positional attribution this test asserts, matching the precedent
  at `crates/events/src/lib.rs:972`).
- **This is #251's real complaint, and it is still open.** #251's *specific* claim (the
  `gridfpv-server` test doesn't compile) is stale — it compiles and now **passes** against
  dockerized RH. But its stated cause — "it's live-gated so `cargo xtask ci` stays green and
  it slipped through" — just produced a *second* instance in `gridfpv-engine`. The ledger's
  "workspace is healthy, `cargo check --workspace --all-targets` → exit 0" reinforces the
  gap: that check does **not** pass `--features live`, so the entire live-gated test surface
  is invisible to it. Resolve #251 as *close the coverage gap* (get live-gated targets
  compiled in CI even if not run), not as a plain close.

---

# Correction 2 — the tournament carve-out (2026-08-23)

## #68 / #69 / #70 were NOT "never built" — they were built, then deliberately cut

Earlier in this session I concluded from `crates/engine/src/` that the double-elim,
round-robin and multi-main generators "were never written". **That was wrong, and
backwards.** They were written, tested, and then removed on purpose:

```
71d49f2  2026-06-30  refactor!: carve tournaments out for a primitives-first release base
```

> Remove the tournament SURFACE (single/double-elim, round-robin, multi-main,
> chase-the-ace + their viz + the Build-tournament UI) so `devel` is a clean base of
> well-tested PRIMITIVES … All tournament work is preserved on branch
> `tournaments-snapshot` (@350c517) to rebuild on the hardened base later.

32 files deleted, ~9k lines. `origin/tournaments-snapshot` @350c517 still holds
`double_elim.rs` (945), `round_robin.rs` (869), `single_elim.rs` (755),
`multi_main.rs` (679), `chase_the_ace.rs` (421) and all five live tests.

**Correct disposition for #68/#69/#70: deferred-by-design, not unstarted.** They should
say so and point at `tournaments-snapshot@350c517`, so the next person doesn't
re-implement from zero. `status/review` stripped from all three (they are not awaiting
review); they stay open.

## This also explains the broken `xtask live`

The carve-out deleted the five engine live tests but **did not trim the matching lines
from `xtask/src/main.rs`**. So `cargo xtask live` has referenced five nonexistent targets
since 2026-06-30 and has exited 1 ever since — an incomplete cut, not rot. (Left alone
for now by decision; it is a 5-line deletion whenever it is wanted, and the issues now
carry the intent instead.)

## Branch inventory — nothing is parked

All non-default branches are **fully merged into `devel`** (zero branch-only commits):

| branch | head | branch-only commits |
|---|---|---|
| `origin/main` | b8b9054 | 0 (level with tag) |
| `origin/tournaments-snapshot` | 350c517 (2026-06-29) | 0 — ancestor; preserved as a *marker* |
| `origin/milestone/v0.4` | 75537e7 (2026-06-20) | 0 — stale marker |
| `origin/milestone/v0.3` | be3336b (2026-06-20) | 0 — stale marker |

So there is **no unmerged work anywhere**. `tournaments-snapshot` is an ancestor of
`devel`, i.e. the carved code is recoverable from history regardless; the branch is a
convenience label for the pre-cut tree.

## The actual road to a usable release

`v0.4`'s vertical slices, per the board's 4 open Todo cards:

| # | slice | note |
|---|---|---|
| #75 | Slice 4 — Heat building from the roster | |
| #76 | Slice 5 — Run a heat (live), hardened | server-authoritative clock supersedes #62 |
| #77 | Slice 6 — Marshaling & results UI | folds in #67 penalties (DQ/time-added/void) |
| #78 | Slice 7 — Multi-heat formats (select + progress) | **body is now stale** |

**#78 needs rewriting.** It reads "pick a format (timed-qual, single/double-elim,
round-robin, multi-mains, ZippyQ — **all built**)". Post-carve-out only `timed_qual`,
`head_to_head`, `open_practice` and `zippyq` exist (and zippyq is shelved per #218, kept
registered but not offered). As written, #78 asks for a picker over formats that are no
longer in the tree — it should be scoped to the primitives that survived, with the
tournament formats returning via the #68/#69/#70 rebuild.

---

# Slice reconciliation — #75–#78 vs. what the alpha shipped (2026-08-23)

Verdicts against the code, using the ledger's own method (acceptance criteria, not titles).

## #62 — server-authoritative race clock → **CLOSE**

Shipped. `LiveRaceState.started_at` (`crates/server/src/live_state.rs:89`) is the
`recorded_at` of the `Armed → Running` transition on the **server wall clock**, documented
as "the anchor every client clock counts from — header and HUD alike".
`crates/server/src/app.rs:308` names it "Server-authoritative race clock (#62 follow-up)".
This is exactly the issue's ask, and it supersedes the client-side clock #76 mentions.

## #67 — penalties + heat-void → **CLOSE (engine side; display is #77)**

Shipped in the engine/event model:

- `Penalty::{Disqualify{reason}, TimeAdded, PointsDeducted, PointsAdded}` (`events/src/lib.rs:371`)
- `Event::HeatVoided { heat }` (`events/src/lib.rs:779`), reversible ("void the void")
- Scoring honours both: `Placement.disqualified` sinks a DQ below every non-DQ competitor
  (`engine/src/scoring.rs:117`), `HeatResult.voided` nullifies a heat (`scoring.rs:183`)

Applying penalties is wired through `control.ts` / `Marshaling.svelte` / the audit trail.

## #75 — Slice 4, heat building from the roster → **CLOSE (verify seat assignment)**

The criterion is "schedule a heat by **SELECTING registered pilots**, *not inline-add*".
That is how it works now: rounds seed `SeedingRule::FromRoster` and
`crates/server/src/control_handler.rs:488` describes "FillRound's `FromRoster` field and the
console's **eligible-members picker**". Round generators lay heats down from the registered
roster.

**Dead code found:** `frontend/apps/rd-console/src/lib/heat.ts` (96 lines) is the *old*
inline-add builder — "type a few pilot names, and those names become both the heat's lineup
and the pilots they're registered to", i.e. precisely the behaviour #75 exists to replace.
It is imported by **nothing but its own test** (`tests/heat.test.ts`). Delete both, or keep
it deliberately as the sim-only path and say so in its docs. Right now it reads as a live
second way to build heats, which is misleading.

## #76 — Slice 5, run a heat (live), hardened → **likely close, needs a criteria read**

The named pieces exist: the heat loop phases (`LiveRaceState.phase`, `HeatPhase`), live
per-pilot laps (`progress`), the live leaderboard (`running_order`), and the
server-authoritative clock (#62, above). Post-alpha commits `8031008` (RH start-sync,
stage retry, stale-replay gate) and `b0dea5d` are hardening work on this path.
"Hardened" is not a checkable predicate, so this one needs *your* judgement on whether the
live path is where you want it — the mechanical criteria are met.

## #77 — Slice 6, marshaling & results UI → **PARTIAL, real gap**

Marshaling itself is done (void / insert / adjust / split, penalties, protests, audit).

**But the results UI never surfaces penalties, and that is a user-visible defect.**

| layer | carries DQ? |
|---|---|
| engine | yes — `Placement.disqualified`, ranks DQ last |
| wire / generated TS | yes — `bindings/Placement.ts:50` `disqualified?: boolean` |
| `Results.svelte` | **no — zero references to `penalt`/`disqualif`/`DQ`** |
| any console file | **no consumer of `Placement.disqualified` anywhere** |

So a disqualified pilot is silently ranked last with **no indication why**. To an RD the
results just look wrong. The heat-level `voided` flag is unsurfaced in Results for the same
reason. (The only `.voided` use in the console is `CompetitorLaps.voided` in
`Marshaling.svelte` — voided *laps*, a different thing.)

This is the residual work #77 should be edited down to: **render `disqualified` / `voided`
in `Results.svelte`.** The data is already there; it is a display-layer job.

## #78 — Slice 7, multi-heat formats → **PARTIAL + body is stale**

The body claims "single/double-elim, round-robin, multi-mains, ZippyQ — **all built**".
After the carve-out (`71d49f2`) the tree offers `head_to_head`, `open_practice`,
`timed_qual` (zippyq registered but shelved, #218). The progression *primitives* survived —
`Generator::advancers`, `SeedingRule::FromRanking`, `RoundStanding` / `ClassStanding` /
`EventOutcome`, round + per-class standings in `lib/results.ts` — but the bracket /
multi-main progression **views** were carved with the generators.

Rescope #78 to "pick a surviving format and drive a multi-round event to standings", and
let the tournament formats come back with #68/#69/#70.

Minor: `lib/formats.ts:4` still cites `double_elim` as an example registry key — stale
doc reference post-carve-out.

## Summary

| # | verdict |
|---|---|
| #62 | close — shipped |
| #67 | close — shipped (engine); display belongs to #77 |
| #75 | close — shipped; delete/annotate dead `lib/heat.ts` |
| #76 | close pending your read of "hardened" |
| #77 | **partial** — results UI must render `disqualified`/`voided` |
| #78 | **partial** — rescope to surviving formats; body actively misleading |

---

# Correction 3 — #77 is NOT a display-layer fix (2026-08-23)

I earlier described the #77 gap as *"the data is already on the wire; it is a display-layer
job."* **That is wrong for the surfaces an RD actually looks at.** Tracing it properly:

| type | rendered where | carries DQ / voided? |
|---|---|---|
| `Placement` | **nowhere on screen** — only `lib/results.ts` → JSON export | yes (`disqualified?`) |
| `HeatResult` | **nowhere on screen** — `Results.svelte` uses `heatResult` only in `exportAll()` | yes (`voided?`) |
| `RoundStanding` | round standings table | **no such field** |
| `ClassStanding` | class standings table | **no such field** |

So:

1. There is **no per-heat placement table in the console at all** — the visible results are
   round standings and class standings.
2. The two types those tables render, `RoundStanding` and `ClassStanding`, have **no**
   `disqualified` / `voided` field, so the information cannot be displayed without changing
   the Rust types and regenerating bindings.
3. The JSON export *does* carry `disqualified` (`results.ts:89` spreads the placement), so
   the data is reachable — but only by exporting and reading the file.

**This is a cross-stack change, not a UI tweak**, and it needs a design decision I should
not make unilaterally: *what does "disqualified" mean at round-standing level?* A pilot DQ'd
in one heat of a five-heat round is not "disqualified" from the round — they simply score
worse, which the ranking already reflects. Plausible shapes:

- **(a)** flag a standings row when any constituent heat carried a DQ/void for that pilot
  (needs a new field on `RoundStanding`/`ClassStanding`);
- **(b)** add a per-heat results view to the console — where `Placement.disqualified` and
  `HeatResult.voided` already exist and would render with no wire change;
- **(c)** both — (b) for the detail, (a) for the at-a-glance marker.

**(b) is the smallest honest fix** and needs no protocol change: the heat-level types already
carry everything, and "the RD marshals a heat, then looks at that heat's result" is the
workflow where a DQ most needs to be visible. Left for a decision rather than guessed at.

What the engine does today is correct and unchanged: a DQ'd competitor is ranked below every
non-DQ competitor, and a voided heat is nullified. The defect is purely that the RD is never
*told* — the ordering changes with no visible cause.

---

# Session log — 2026-08-25: matrix green, #355 slices land

## The matrix is green on `v0.4-355-397`

`cargo xtask live` — **24 green, 0 failed**, all four legs (RH 4.4.0 / 4.3.0 × plugin /
no-plugin). This is the first full-matrix green since the four-leg matrix was introduced for
#389, and it is on the branch carrying the #397 and #355 work rather than on `devel`.

## Merged to the branch

- **#397** — complete: the crossing feed plus tones, firing on `Armed` as well as `Running`.
- **#355 slice 1** — `RssiGraph` generalised into review + live modes (1071 → 1017 lines,
  with the pure half extracted to `lib/rssiGraph.ts`).
- **#355 slice 2a** — the tune telemetry pipeline and the leased `GET /timers/{id}/signal`.
- **#355 slice 2b** — the per-timer Tune page at `#/timers/<id>/tune`.
- **#409** — the min-lap floor applies on every live fold; D26 is now enforced by a
  nine-surface conformance test.

## Two real gaps found on merging 2b — both being fixed

Slice 2b was written while 2a was unmerged, and the two do not meet.

**1. The wire types were fabricated.** `tuning.ts` hand-declared `TimerSignal` /
`TimerSignalNode`, and **every field name is wrong** against what 2a actually shipped:
`current_rssi`→`rssi`, `enter_at_level`→`enter_at`, `crossing_flag`→`crossing`, per-node
`from`/`period_micros`→ a shared top-level `sample_micros`. The page would have rendered
every readout as `undefined` against a live Director.

All five gates passed green on this. They are structurally unable to catch it: `tsc` cannot
tell a fabricated interface from a real one, the tests used fixtures shaped like the
fabrication, and the contract suite never exercised the signal endpoint. Filed as **#410**
with a proposed guard (barrel-completeness check + contract coverage), because the incentive
to guess a shape recurs on every slice built against an unmerged sibling.

Underneath it: `TimerSignal`/`NodeSignal` were generated into `bindings/` but never added to
the `@gridfpv/types` barrel, so they were **not importable** even by an author who wanted to
do the right thing. That is the actual cause and is fixed with the symptom.

**2. The write endpoint does not exist.** 2b invented `POST /timers/{id}/calibration` and a
`CalibrationReadback` type. The real routes are `connect`, `disconnect`, `restart`, `signal`,
`signal/stop` — there is no way to set a threshold at all, so the page is read-only and #355
is unfinished. The adapter declares `Capability::Calibration` and *reads* thresholds, but
nothing ever writes one.

Being built now on the restart (#386) pattern: registry queue → drained in
`rh_connections.rs` → socket.io emit in `transport.rs`, gated on connected + RotorHazard +
no heat in progress. **Confirmation is by poll, not by response** — RotorHazard does not echo
a level set synchronously, it broadcasts `enter_and_exit_at_levels`, which already flows back
as `NodeSignal.enter_at`/`exit_at`. A synchronous fake readback would report success for a
write that never landed, which is precisely the failure this page exists to diagnose.

Also corrected: the write goes through `@gridfpv/protocol-client` + the session token, not a
bare `fetch`. The route is behind `ControlAuth`, so a bare POST would 401 on any gated
Director — including the RD's real one.

## #406 in flight

`RawCurrentLaps.lap_number` is typed `u64`, so RotorHazard's `-1` late-lap marker fails
**the entire frame** rather than the one lap. Measured: `undecodable_frames=4,
rh_deleted_laps=0`. The consequence is that #400's deleted-lap counter — added specifically
to make #403 visible — never fires, and the malformed-frame counter fires instead, pointing
a field diagnosis at "schema drift" instead of "RotorHazard stopped counting".

Only affects the legacy fallback path (#405's owned format means RH never declares a winner
and never emits `-1`). That is *why* it is worth fixing: the fallback is what runs when
something has already gone wrong, and it is the path whose diagnostics we most need to trust.

## Still open, still needing your input

**#402** (practice channels), **#407** (RH min-lap filter + RHAPI audit), **#408**
(`EventOutcome` unserved), **#410** (the guard above — which of the three options).

---

# Session log — 2026-08-26: the Tune page proves out on hardware

## First real-RF validation of #355

The RD ran the Tune page against a live NuclearHazard: *"the tuning is amazing, UI/UX is exactly what I want, it seems to work — can validate the values change on RH with a page refresh."*

Persisted calibration read back off disk afterwards, which is D27 working:

```
NuclearHazard   node 0 → 62/35   node 1 → 68/57   node 2 → 68/57
```

Node 3 is absent because it was never tuned — the record is of *decisions*, not a snapshot.

**And it immediately caught real drift.** Grid's record said node 0 enter = 62; the timer reported 37. Either a later write never landed, or something moved it after Grid recorded 62 — and Grid cannot currently tell which, because D27's re-apply half is deliberately unbuilt (#355 flagged it). The case the drift notice exists for occurred naturally within an hour of the feature shipping.

## Closed today (22 issues)

Highlights beyond the routine:

- **#412** — node count is now discovered from the timer. A real 4-node timer was configured as **8**, and `round_engine` caps pilots on that number, so Grid would have seated 8 pilots on a timer that can time 4. Found a fourth discovery source nobody had read: the plugin's `gridfpv_hello_ack.node_count` was already parsed and unused.
- **#410** — the fabricated-types guard. `wire-shape.ts` validates live responses against the *binding files themselves*, so the test has no opinion of its own to be wrong. Fed the original fabrication, it names all nine divergences. Option (2) was deliberately **not** built: it would not have caught the bug it was proposed for, since `TimerSignalNode` was a name that never existed.
- **#418 / #416** — the RD's own practice round was simultaneously mis-seeded onto a nonexistent node *and* undeletable, with the refusal recommending a route that does not exist. Both fixed; stored rounds are now validated on **read**, not only on write.
- **#84** — closed as *"what exists is better than what was proposed"*. `RoundDef` already carries per-round format, classes, win condition and `FromRanking` carry; the built model expresses progression as a **DAG**, which the proposed per-class ordered list cannot do at all.
- **#97** — closed as already built; `EventSetupWizard` has been wired since before the issue was revisited.

## The empty-pool trap, five times over

`available_channels` is **empty on every real RotorHazard timer**, because they report `Flexible` — which means *"no restriction"*, not *"no channels"*. That single ambiguity produced five separate defects:

1. `round_engine.rs:273` returns early → **channel assignment has never once run on real hardware**, so the IMD machinery below it has never executed
2. `TimerManager.svelte:147` → renders *"No channels available"* for a timer that can do all 52
3. #413's dropdown would have rendered empty on exactly the timers it is for
4. #416's seat labels degraded to `Node 7` with no channel
5. #402 is almost certainly the same root cause

Fixed at each site; #117 S1 removes the cause by making the pool **event config** rather than timer-derived.

## #117 reframed by the RD

The RD rejected the flat-pool model in favour of three scopes: **global** (what may this timer ever use) → **event layers** (which channel on which node) → **heat** (pick a layer, timer retunes).

This dissolved an objection rather than answering it: I had assumed a pilot's channel should determine their heat, which would have broken bracket filling. It does not — the RD picks the strategy. A bracket is *one layer all tournament*; a GQ qualifier uses many layers to keep pilots put. Both fall out of one mechanism.

IMD moves from **chooser to checker**, validating a layer at definition time — paid once, and at the only moment the RD can still act on it. Clever pilot-channel assignment deferred to **#419**.

## Still open for the RD

**#402** (folds into #117 S1), **#405**, **#407**, **#415**, **#421**, **#355**'s remaining ADDs (Capture button, min-lap warning), **#397**'s pre-race tones (needs the telemetry source, not the log).

## Overnight run — 2026-08-26 into 27

Matrix **4/4 PASS** (800s) on the finished tree. `cargo xtask ci` exit 0; frontend 970 tests + 113 contract.

Merged: **#417** (lap callouts), **#415** (gate signal strip), **#407 part 1** (min-lap filter), **#414** (Practice event removed). Closed 26 issues on the day.

### Two dead code paths found by doing the work

**`set_min_lap_time` never did anything.** It emitted `set_option {option: "MIN_LAP_TIME"}` — but RotorHazard has *no such setting*. That string is only an event-constant name; the filter reads `MinLapSec` (a DB option) and `TIMING`/`MinLapBehavior` (a server-config item), in two different stores. RH's `on_set_option` writes whatever key it is handed, so the call stored a row nothing reads and returned `Ok`. It survived because RH's default behaviour is highlight-not-discard, so the harness's short sim laps recorded anyway and nothing ever contradicted it. The same dead emit sat in `race_day.py`, and `xtask rh-mock` recommended it to RDs.

Second time a name has lied, after `unlimited_time`/`race_mode`. That is now the standing warning on **#423**: *a dead write that returns success is the failure mode the audit exists to find.*

**The #417 dip was not the min-lap floor.** D26/#409 is holding — every live producer is floored, so a too-short pass arrives already `RejectedTooShort` with no `lap_number`; the count never rose and never fell back. What dipped was a **reconnect replay**: the stream's resume cursor is a deliberate lower bound (the client advances `+1` per *applied* envelope because the wire echoes no per-envelope offset), so a socket blip replays intermediate folds. Filed as **#422**.

The client fix stands regardless, and that is the point — a consumer keyed on identity does not need to know why a count moved. `useLapCallouts` was the last count-keyed detector in the console.

### Removing Practice took its scaffolding with it

`list()` pulled it out first, `create()` dodged its reserved id, `delete()` refused it, `restore_persisted_events()` skipped a stray `practice.sqlite`, the picker split the list in two, and the contract harness defaulted to it. All of that existed only to keep a **test fixture that shipped to users** working. Tests now create their event through the real API, which is better coverage than the fixture ever gave.

### Filed, not built

**#421** (heat setup sends no band/channel label to RH — the other write path #413 could not reach), **#422** (the reconnect replay), **#423** (#407 part 2, the RHAPI audit), **#424** (the matrix's no-plugin legs should assert Grid *refuses* to race — #405's own warning is that deleting them recreates #389's blind spot).

### The bench, left ready

Practice event gone, the RD's `test-d46ptx` event intact, NuclearHazard and Docker RH restored with the 3 calibration entries (they had been left behind in the pre-fresh-start data dir). Both timers left **Configured, not connected** — #407's neutralisation writes to the timer on connect, and that should be the RD's choice to make while watching.

---

# Session log — 2026-08-27: the board gets honest

v0.4 went from **20 open to 12**, with 73 closed. Most of the reduction was not new work — it was discovering that four issues had already been delivered by other means, and that two more were asking for something the design had outgrown.

## Closed as already-built

- **#42** (snapshot endpoints + scope grammar) — the scoped GETs and cursor have existed and been tested for weeks; #422 then made the stream cursor exact.
- **#53** (bind a pilot to a `node-{i}` seat) — delivered sideways by S3's seat-first seating editor. The issue wanted a bind *action*; what shipped is a seat that simply carries an optional pilot everywhere. The Director never needed changing — `validate_tagged_lineup` had always accepted node refs; **only the UI required a pilot**.
- **#65** (`rh:<url>` lap source) — overtaken. The four-leg live matrix drives RotorHazard's *real* detection pipeline, which is strictly more realistic than the `simulate_lap` path this asked for.
- **#78** (multi-heat formats) — closed at the RD's direction; replaced by **#431**, a full rescope of tournaments on the primitives, rather than resuming a slice whose assumptions #84 and #117 have already outgrown.

## Two corrections I had to make about my own claims

**#80 — I said the shipped app was exposed. It is not.** I read `DEFAULT_ADDR = "0.0.0.0:8080"` in `director.rs` and asserted the Tauri app inherited it. It does not: `src-tauri/src/lib.rs` binds **`127.0.0.1:0`** and documents why (*"Loopback ⇒ the control path is open (no auth), per the GridFPV auth model"*). The loopback-trust half of #80 is already implemented for the surface that ships. Corrected on the issue.

**#427 — my diagnosis was wrong, and the real problem was far larger.** I blamed #414's fixture rewrite and a console 400 storm. The cause was commit `8ad6344` renaming a nav item **"Live control" → "Race control"**, leaving a stale locator in an `enterPractice` helper copy-pasted into ~15 spec files. Not two specs: **38 of 46**, every one timing out at navigation before touching what it was written to test.

It hid because **the suite could not run anywhere** — no Chromium system libraries on the build box, and `ci.yml` said browser e2e was *"gated separately"*, which meant nowhere. #414 then edited every e2e fixture with nothing executing them.

## e2e can now run, and does

`cargo xtask e2e` runs the suite in Microsoft's Playwright image. The design collapsed once tested rather than assumed: **the host-built Director binary runs unmodified inside the image** (image glibc 2.39, host 2.43 — a Rust binary needs the symbols it *uses* to exist, not an identical libc), so no custom image and no Rust in the container. A CI job runs the same specs on every push.

21 specs pass; the rest are stale against the layout work and the rename (**#428**).

## The IMD metric is wrong, and the picker acts on it

Research against IMDTabler (validated to reproduce its published ratings exactly) found `imd_score` is **non-monotonic against the standard**: MultiGP's official 6-pilot set, RotorHazard's default, and a perfect-100 Raceband-4 set all score **0** — the same as all-eight-Raceband, the worst set in FPV.

Cause: the three-tone term hits exactly zero whenever two pairs share a sum, which is what a balanced set looks like. **The metric punishes good sets for being symmetric.**

And `pick_best_imd_set` maximises it, so it prefers sets the standard rates poorly — its best 6-set has two channels **15 MHz apart**, unflyable on adjacent-channel bleed alone. #117 S1 made that picker execute for the first time, so this went from dormant to live. Filed as **#430**; the fix is to port IMDTabler's rating, which is also what RotorHazard shows the RD.

## Standing rule added

`CLAUDE.md` gained **"Pre-release: break things freely, and do not write migrations."** Agents were writing old-key fallbacks for data nobody has. Bounded: a record in an old shape must still *load*, data the RD cannot recreate is still preserved, and a change that drops data must say so. It names the condition that flips it — the first public release — and instructs its own deletion then.
