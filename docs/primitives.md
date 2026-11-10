# Grid primitives

**Status:** revision 7 (2026-11-09): the design to build against, with build progress in §8.
Revision 1 was written from the design discussion of that day; revisions 2 to 7 fold in
Ryan's notes and answers. Sections marked **Agreed** record what Ryan decided.

**This is the current design.** Every other doc in this folder is either marked old or marked
under review (§10). Where another doc disagrees with this one, this one wins.

To change it, add `//NOTE:` lines anywhere, as before.

**What changed in revision 2:** "seat" replaces "lane"; a class is not a phase (§5.3);
"format" is the named, reusable bundle of rules and is inherited down the tree (§5.6); rules
are data so custom ones can be defined (§5.5); presets leave a display hint (§6); a false
start always means a restart (§5.7); the run queue (§5.9); the roster replaces "entry" (§4).

**What changed in revision 3:** a ranked phase serves exactly one class (§5.3); pilots who
never flew rank below everyone who did (§5.5); teams are planned for but not built (§4); all
the questions from revisions 1 and 2 are answered.

**What changed in revision 4:** the doc audit added §5.10 and twelve questions, now all
answered. A saved race can be discarded (§5.1); a phase can be unlocked and protests block a
lock (§5.8); there is one way to remove a lap (§5.7); a practice seat can be left open
(§5.4); a seat has an address that depends on the timer (§3.2); classes and tracks outlive
events and pilots carry outside ids (§4); a field can be seeded from an imported list
(§5.2); OFT is used from the local checkout for now (§8).

**What changed in revision 5:** removing a lap always folds its time into the next lap; a
restart point is the separate case (§5.7).

**What changed in revision 7:** the six questions from building the engine are answered (§9
says where each answer went). A disqualified pilot's earned place passes down by default and
the director can keep it empty (§5.7). What starts the first lap is a race rule, hole shot by
default (§5.6). Brackets are generated for any field and heat size (§6). A lone pilot may fly
or be waved through (§5.4). A position override is set on a heat (§5.7).

---

## 1. Why this doc exists

Grid's earlier model called three round types (Practice, Time Trial, Head-to-Head) its
primitives. They were presets. Each one bundled three separate decisions: who flies together,
how one race ends, and how results become a ranking. The engine grew a generator per type, and
every new shape (open-ended runs, rotations, brackets, mains) needed a special case.

grid-rh had the same problem from the other side. It inherited RotorHazard's shape, where a
"heat" is a group and a "round" is a re-run of that group.

The rule from here on (**Agreed**):

- **Build a small set of primitives. Build everything else on top of them.** Event types,
  brackets, mains, single pilot mode and UI modes are all compositions.
- **Build for club racing.** Single pilot racing is a simplified mode on the same primitives,
  never a separate kind of event.
- **RotorHazard is not the reference.** Its vocabulary and structure are used only inside
  the RotorHazard adapter.
- **Grid works across all timers.** No primitive may assume one timer's behaviour.
- **Never down-convert data.** Keep the most granular value the source gives.

---

## 2. The primitives at a glance

| Group | Primitive | One line |
|---|---|---|
| Instruments | **Timer** | A device that reports crossings. Controlled, never consulted. |
| | **Seat** | One pilot's timing slot on a timer: a channel plus thresholds. |
| | **Profile** | A saved set of seat settings for a timer. Lives outside any event. |
| | **Pass** | One crossing: timer, seat, gate, time on the timer's clock. |
| | **Signal samples** | The raw signal behind the passes, in the source's own units. |
| People | **Pilot** | A person in the persistent roster. |
| | **Class** | A name plus the pilots racing in it at this event. |
| Competition | **Race** | One start-to-stop flight. The atom. |
| | **Phase** | Field in, ranking out. Nests freely. |
| | **Heat** | The leaf phase: a lineup that is raced one or more times. |
| | **Ranking rule** | A pure function that turns results into a strict order. |
| | **Completion rule** | Says when a phase is done. |
| | **Format** | A named, reusable bundle: race rules, completion rule, ranking rule. |
| | **Seat reference** | A place in a lineup: a pilot, or "position N of phase X". |
| | **Ruling** | A human decision recorded as a fact. |

Underneath all of them sits the part of Grid that does not change: an append-only log, pure
adapters, pure folds, and a race clock set by the server. See §8.

**A word collision to know about.** The old docs use "phase" for two other things: a step of
the event pipeline (setup, practice, qualifying, mains, season) and a state of the old heat
state machine. In this doc **phase** means only the nesting container defined in §5.2.

---

## 3. Instruments

### 3.1 Timer (Agreed)

A timer is an instrument. Grid owns every setting and every record. Grid writes settings to
the timer and never reads them back as truth (old decisions D26 and D27, carried forward).

Timers differ in who owns the start of a race. This is recorded on each race (§5.1):

| Timer | Who starts the race | What Grid tells the timer |
|---|---|---|
| OFT | Grid | Nothing. The timer always listens and always reports. |
| RotorHazard | Grid | Start and stop, because RotorHazard needs them. |
| Velocidrone | The sim | Nothing. Grid follows the sim's race. |

OFT is the model of a well-behaved timer: Grid updates its config and receives crossings and
samples. Grid never tells it a race is starting or stopping.

### 3.2 Seat and Profile (Agreed)

- A **seat** is one pilot's timing slot on a timer. It has a channel and thresholds (fixed
  enter/exit values, or automatic).
- **A seat is not a receiver.** A timer says how many seats it has, and how it serves them is
  the timer's business. One OFT receiver can serve several seats by hopping between
  channels, at a lower sample rate per seat. OFT reports each seat's real sample rate, and
  Grid shows it so the director can see the cost of adding seats.
- **A seat has an address, and its form depends on the timer.** On a video timer it is a
  channel. On Velocidrone it is a player name. On a transponder timer it is a transponder id.
  Players a sim sees are offered for adding to the roster. They are never added silently.
- A **profile** is a saved set of seat settings for a timer.
- Seats and profiles belong to the **timer**, outside any event.
- A heat's lineup puts pilots on seats (§5.4). That lineup **is** the link between a pilot and
  a seat. There is no separate registration step.

This replaces the three-scope channel layout model (allowed set on the timer, per-event
layouts, per-heat binds and overrides).

Tuning follows grid-rh's Tune page: record a seat for as long as the director wants, draw the
trace, and adjust the thresholds against it.

### 3.3 Pass and signal samples (Agreed)

- A **pass** is one crossing on one seat at one gate. Its time is on the timer's own clock.
- Lap times are differences between pass times on that one clock. The time a pass arrived at
  Grid never affects a lap time.
- **Signal samples** are stored in the source's own units at the source's own resolution. OFT
  reports tenths of a dB; RotorHazard reports counts. Each stream carries a unit tag. Nothing
  is converted down.

### 3.4 Discovery (Agreed)

- The **Timers page lists every timer found by mDNS**, with a Connect button on each.
- A laptop, a timer and a plain switch have no DHCP server, so discovery must work on
  link-local addresses. IPv6 link-local is always present; IPv4 link-local is the fallback.
- A typed address always works as a last resort.

---

## 4. People

- **Pilot** (Agreed): a person in a roster that outlives any event. A pilot may carry ids
  from other systems, such as a MultiGP id and a sim handle.
- **Classes and tracks outlive events too** (Agreed). They are app-level lists, like pilots.
  An event picks its classes and its track from them. This is what lets a season treat
  "Open" at two events as the same class, and tells Velocidrone which track is flown.
- **Class** (Agreed): a name plus its **roster**, the pilots racing in that class at this
  event. A class is not a phase (§5.3).
- **Seed:** a pilot's position in a phase's field (§5.2).

A class simply has a roster (Agreed). There is no separate "entry" record.

- **Check-in** comes later. When it does, a pilot joins a class roster at check-in.
- **Teams** may come later, and the design leaves room for them (Agreed): a team is a named
  group of pilots at an event. Lineups, passes and races stay about pilots. Team results are
  a ranking rule that combines the results of a team's members. Nothing in §5 has to change
  to add this.

---

## 5. Competition

### 5.1 Race (Agreed)

A race is one start-to-stop flight. It is the atom. Every race has its own id and belongs to
one heat.

**States:**

```
ready → running → stopped → saved → discarded
                          → discarded
```

- The staging countdown is part of the start. It is not a state.
- **The director must save a race.** A stopped race is undecided until the director saves or
  discards it. No other race can start until they do.
- A save may carry a **needs marshal** flag and a note.
- A discard may carry a **reason**: false start, crash or obstruction, timer problem, or
  other. **The reason is optional and is one tap to pick.** A note is optional too.
- **A saved race can still be discarded**, in one step, until a phase that contains it is
  locked. The director does not have to un-save it first. This is how a false start called
  after the race is handled.
- A discarded race stays in the log. It does not count toward any ranking.

**Facts in the log:**

| Fact | Carries |
|---|---|
| Race started | race id, heat, lineup, who owned the start |
| Race stopped | race id |
| Race saved | race id, optional needs-marshal flag and note |
| Race discarded | race id, optional reason and note |

**Restarts** (Agreed): there is no "restart" type. A restart is a discard followed by the next
race of the same heat. The reason lives on the discard.

**Race rules** are the settings for one race. They are not set on the race. They come from the
format in force for the heat (§5.6).

- Start procedure: staging time, random hold, start tone. The console plays the tone.
- End condition: first to N laps, a fixed time plus a grace window, or open until stopped.
- Lap filters: minimum lap time, and "ignore crossings before" at the start.

### 5.2 Phase (Agreed)

A phase takes a **field** in and gives a **ranking** out. A phase may hold other phases, to
any depth. The same shape is used at every level: qualifying, a main, a bracket round and a
heat are all phases.

- **Field:** an ordered list of pilots. It is built from one or more slices, each a class
  roster, a pilot, "positions A to B of phase X", or an **imported list** (a Velocidrone
  leaderboard or a MultiGP list). The order of the field is the seed order, so a bracket can
  be seeded from an imported list with no qualifying flown.
- **Ranking:** a strict order of the field. Each place also carries the result behind it
  (laps, time, points, wins), so that a parent phase can combine rankings.
- **Label:** the director's own name for it (Qualifying, C-Main, Winners Round 2). The word
  "phase" appears in code and docs, rarely in the UI.

Free nesting is the model. Common shapes are offered as presets (§6) so nobody has to build a
tree by hand.

### 5.3 Event, class and phases (Agreed)

An event holds classes and phases. A class is not a phase.

```
Event
├─ Classes:  Open (roster),  Spec (roster)
└─ Phases
    ├─ Practice           serves Open and Spec
    ├─ Open Qualifying    serves Open
    ├─ Open Mains         serves Open
    ├─ Spec Qualifying    serves Spec
    └─ Spec Mains         serves Spec
```

- Each class generally has its own set of phases (qualifying, mains).
- **A ranked phase serves exactly one class.** The same pilots are often in several classes,
  so results are kept apart by class. There is no combined qualifying.
- Only an unranked phase can serve more than one class. Shared practice is the case.
- **Class standings** (Agreed): each class lists the phases that count toward its result,
  in order. The first one decides: for a class with mains, the Mains ranking is the class
  standings. An earlier phase is only **reached back to** when the first leaves pilots
  unplaced or level, for example pilots who never made a main are ordered by Qualifying.
  Practice never counts. This is what gives every class a ranking from 1 to n.

### 5.4 Heat (Agreed)

A heat is the leaf phase. It holds a **lineup** and an ordered list of **races**.

- A heat can be raced more than once. Restarts, repeat runs, points over several races and
  Chase the Ace all use this.
- A lineup has one **seat reference** per seat: a named pilot, or "position N of phase X". A
  reference to an unfinished phase shows as its description ("Winner of Heat 3") until it
  resolves.

**Open seats in practice:** in an unranked phase a seat may be left open, with no pilot
named. Whoever flies on it is timed, and nothing is ranked.

**Channels when a lineup resolves:** each pilot keeps the channel they had in their previous
heat where possible. When two pilots in a heat collide, the **lower seed moves**. The
director can override the result, and the override is a ruling.

**A pilot who does not show:** they have no laps, so they rank last in the heat. Several
no-shows are ordered by seed. The director may also remove them from the heat, which is a
ruling. Removing them does not improve their place: see the no-show rule in §5.5.

**A pilot alone in a heat** (Agreed): byes in a bracket can leave one pilot in a heat. That
is the director's and the pilot's choice. The pilot can fly it, or the director can call the
heat done and the pilot advances without flying. The director can call any heat done this
way, whatever its completion rule says.

### 5.5 Ranking rules and completion rules (Agreed)

A **ranking rule** is a pure function. On a heat it reads the heat's saved races. On any other
phase it reads the rankings of its children.

Rules for a heat:

- finish order
- most laps
- best lap
- best N consecutive laps
- most wins across its races (Chase the Ace)
- **points summed across its races** (the pro-spec style: one heat flies N races, each race
  scores points, and the total decides the heat)

Rules for a phase with children:

- **stack in order:** place pilots by the highest child they reached (mains, brackets)
- **sum points** (round robin, season)
- **best result** across children (time trial over several rounds)

Three guarantees hold for every ranking rule:

1. **A ranking is a strict order.** Every rule ends in a tie-break chain. The last link is
   always the pilot's seed coming into the phase. For a bracket the chain is: finish position
   in the heat where the pilot went out, then seed.
2. **Pilots who never flew rank below everyone who did.** In any phase, a pilot with no
   race flown in that phase is placed after every pilot who flew, whatever their seed. Among
   themselves they are ordered by seed. In a 16 pilot bracket where the 8th seed misses
   their heats and everyone else races, the 8th seed finishes 16th.
3. **A ranking contains every pilot in the field exactly once.** Class standings therefore
   run from 1 to n, where n is the number of pilots in the class. Every preset is tested for
   this.

A **completion rule** says when a phase is done.

| Rule | Used by |
|---|---|
| After one saved race | Most heats, including the final of a lower main |
| After N saved races | Repeat runs, points over N races |
| When a pilot has N wins | Chase the Ace (N = 2) |
| When the director says so | Practice, single pilot runs |
| When every child is done | Any phase that holds phases |

**Rules are data, so custom rules can be defined** (Agreed).
The built-in rules above are not special code. Each is made from the same small set of
**measures** that Grid folds from races and heats:

- per heat: saved race count, time elapsed
- per pilot: laps, best lap, best N consecutive, total time, finish position, wins, points

A completion rule is a condition on measures ("any pilot's wins ≥ 2", "saved races ≥ 3"). A
ranking rule is an ordered list of measures to sort by, ending in seed. The first build ships
the named rules only. A rule editor comes later, and needs no engine change because the
named rules are already stored in this form.

### 5.6 Format (Agreed)

A **format** is a named, reusable bundle: race rules, a completion rule and a ranking rule for
heats, plus a points table when the ranking uses points.

- A format is set on a phase and **applies to every heat beneath it**.
- A phase lower down can set its own. The nearest one wins.

So a director sets "2 minutes, most laps" once on Qualifying. In a multi-main, each main
carries "first to 3 laps, one race", and only the A-Main final heat carries the Chase the Ace
format.

**What starts the first lap** (Agreed) is one of the race rules:

- **Hole shot** (the default): the pilot's first crossing starts lap 1. That crossing has no
  lap time.
- **From the tone:** the start tone starts lap 1, so the first crossing ends it.

**How a first-to-N race stops** (Agreed) is a race rule too. By default the director stops
it. As an option it stops by itself a set wait after every pilot has finished. The wait is
there for a hard track, where extra crossings are likely and the director may need the race
kept open to sort laps out. If a finishing lap is removed during the wait, the race keeps
going. A pilot who never finishes means the race never stops by itself.

**A time limit** (Agreed) can be set on any race that is not timed: first-to-N or open. The
race stops by itself at the limit, whoever has or has not finished. With both a limit and
the wait above, whichever comes first stops the race. The default is no limit.

Timing from the tone needs the tone's time on the timer's own clock. The same is true of the
filter that ignores crossings before the start. A timer that cannot give that time falls back
to the hole shot and filters nothing at the start. OFT can give it.

### 5.7 Ruling (Agreed)

A ruling is a human decision recorded as a fact in the log. Rulings are never automatic.

- Lap corrections: remove, insert, adjust, split.
- **There is one way to remove a lap, and it always behaves the same.** Take laps A, B and C
  in a row. Removing B leaves A as it was, because A was a full lap. C now starts where A
  ended, so B's time becomes part of C. Like a race discard, a removal takes an optional
  reason that is one tap to pick (false detection, incorrect lap, other). The reason is a
  label only and never changes the result. A removal can be undone.
- **Restart point:** the one other thing that can happen to lap B. Marking B as a restart
  means B has no lap time at all, and A and C both stay as they were. A and C can never be
  used together: "best N consecutive" may not span a restart. Always marked by a person.
- **False start:** names the pilot and the race.
- Penalties: time added, points changed, disqualification.
- **Position override:** the director can always set a pilot's position **in a heat**. It
  takes effect from there on: every lineup and ranking that reads the heat sees the new
  order. It holds until the director clears it. Positions in a phase above a heat are never
  set by hand; they follow from the heats.
- Lineup and channel overrides, including removing a no-show.

**False starts:**

- **A false start always means a restart.** If it is called during the race, the race is
  stopped and discarded. If it is called after the full race has run, that race is discarded
  and re-run. A race never stands with a false start in it.
- The false start is recorded against the pilot, so it can be counted.
- A phase may carry a **false start limit**. The count covers every race under that phase.
- When a pilot reaches the limit, Grid shows the count and proposes the disqualification. The
  director confirms it.
- A disqualified pilot is dropped from later lineups in that phase and ranked below everyone
  who kept racing.
- **A place the pilot had already earned** (a bump-up spot they won): by default the next
  pilot moves up and takes it. When disqualifying, the director can choose to **keep the
  place empty** instead. Then nobody moves up, and that seat in the next heat stays empty.
- Default limits for presets: none in qualifying; **2 across a whole multi-main**; 2 across a
  whole bracket.

### 5.8 Official results (Agreed)

Results become official by **locking a phase**. Locking a phase locks everything beneath it.

- A race's result can be protested, and rulings can change it, until a phase that contains it
  is locked.
- **A locked phase can be unlocked** by the director. Unlocking is a ruling and is logged.
- **Protests:** a protest is filed against a race and resolved once. A phase cannot be
  locked while it holds an open protest. There is no timed protest window; the lock is the
  cut-off.
- Single pilot mode never shows the lock or the protest tools.
- When a ruling changes a finished heat that later lineups refer to: lineups that
  have not raced yet update by themselves. A heat that has already raced is flagged for the
  director and is not changed.

### 5.9 The run queue (Agreed)

The director decides the order heats are flown.

- **Any heat whose lineup has resolved can be started at any time.** Grid never forces an
  order.
- Grid keeps a **run queue**: a suggested order the director can drag around.
- The default queue runs one phase to completion, class by class.
- Helpers reorder it on request, for example alternating two classes, or spacing out a pilot
  who flies in both.

### 5.10 Details filled in from the doc audit (Agreed)

The section-by-section audit of the older docs (§10) found things the old design handled that
this doc had not said. These are the ones with an obvious answer. The ones that need Ryan's
call were put to him and are folded in above.

- **A pass is tied to a race.** When a pass is logged while a race is running, it is stamped
  with that race's id. Passes outside any race are logged without one. (Old decision D20.)
- **The seat settings in force are logged with the race.** The race started fact carries
  each seat's channel and thresholds, so a trace can be reviewed honestly later even after
  the profile has changed.
- **One shared countdown.** A "race starting" fact is logged when staging begins and carries
  the planned tone time on the server clock. Every console and overlay counts down from it.
  The race started fact is logged at the tone.
- **A timer says what it can do.** Signal samples, adjustable thresholds, tunable channels,
  several gates, and who owns the start are declared by the adapter. Grid hides what a timer
  cannot do.
- **Sources also report session edges.** A sim reports when its own race starts and ends.
  That is how "the sim owns the start" (§3.1) works.
- **Rulings name their race.** A lap correction or penalty carries a race id. A ruling on a
  discarded race is rejected. A removal can be undone, and a removed crossing is never
  proposed again. (Old decisions D23 and D25.)
- **A format is frozen once it has saved races under it.** To change the rules mid-phase,
  the director sets a new format on the phases still to run. (Old decision D24.)
- **Provisional and official.** A saved race is provisional. A locked phase is official.
  Readers such as overlays and exports can see which. A discarded race is never shown as a
  result and never pushed to an outside service.
- **Tune recordings belong to the timer.** They are stored with the timer, outside any event.

---

## 6. Presets

A preset is a **generator**. It runs once, sized to the field, and writes plain phases, heats,
seat references and formats. The director can then edit any part by hand.

| Shape | Composition |
|---|---|
| Practice | Heats with no ranking rule, done when the director says so |
| Time trial | Lineups from the roster; each heat ranked by best N consecutive; parent takes each pilot's best result |
| Head-to-head round | Lineups from a seed; ranked by points or finish order |
| Points heat | One heat, N races, points summed across them |
| Bracket | A chain of rounds where each lineup refers to earlier heats (see below) |
| Mains | Brackets stacked with bump-ups (§6.1) |
| Chase the Ace | One heat, raced until a pilot has two wins |
| Single pilot | One heat with one pilot, done when the director says so, best 3 consecutive |
| Season | A ranking rule over several events |

**Brackets** (Agreed). One generator covers every shape Ryan runs: single or double
elimination, heats of 2 to 8 pilots, and any field from 3 pilots up. The director picks the
heat size (how many seats), how many advance from each heat (at most half), and single or
double.

- Round 1 deals the field into as few heats as hold it, all the same size or one apart.
  Seeds are spread by a snake: with 8 pilots and 4 seats, 1-4-5-8 race 2-3-6-7.
- The top pilots of each heat advance, round after round, until one heat is left. Two pilots
  out of the same heat are kept apart in the next round where that is possible.
- **Single elimination:** fail to advance and you are out. The last heat is the final.
- **Double elimination:** fail to advance and you drop to the losers side. The losers side
  races down between each drop. The best of the winners final and of the losers final meet
  in the final.
- A field that fits in one heat is just a final.
- Pilots are placed by the last round they flew. Pilots who go out in the same round are
  ordered by their place in their heat, then by seed.
- A 16 pilot double elimination with 4 seats is 14 heats.

**Big events** (70 to 100 pilots) run as mains of 16: the generator takes the number per
main and the number who bump up, and says how many mains the field needs. 96 pilots with 2
bumping up is 7 mains.

**The display hint** (Agreed in aim). The engine never needs to know which preset made a
tree. Views and overlays do: a double elimination bracket, a mains ladder and a points table
each want to be drawn their own way. So a preset stamps the phase it creates with a **display
hint** naming that layout.

- Only views and overlays read the hint. Rankings, completion and seat references never do.
- If the director edits the tree until the layout no longer fits, the view falls back to the
  plain list of heats.
- When the modular overlay system arrives, a custom overlay picks its layout from the same
  hint.

### 6.1 The hardest case: multi-main with double elimination (Agreed)

This is the test the model was checked against.

```
Open (class)
├─ Qualifying                  no false start limit
└─ Mains                       false start limit 2; ranking: stack the mains
    ├─ D-Main  (double elimination; final is one race)
    ├─ C-Main  (field = a qualifying slice + D-Main top n)
    ├─ B-Main  (field = a qualifying slice + C-Main top n)
    └─ A-Main  (field = a qualifying slice + B-Main top n; final is Chase the Ace)
```

- Each main is a double elimination bracket: phases of heats whose lineups refer to earlier
  heats.
- The top n of each main bump up into the next one. That is a field built from two slices.
- The finals differ only by format:

| Final | Completion rule | Ranking rule |
|---|---|---|
| D, C and B mains | after one saved race | finish order |
| A-Main | when a pilot has two wins | wins, then finish order |

- The class standings run across all the mains, 1 to n:

| Places | Who |
|---|---|
| 1 to a | A-Main's ranking, including pilots who bumped up into it |
| next | pilots who finished in B-Main and did not bump up, in B-Main order |
| next | the same for C-Main, then D-Main |
| last | pilots in the class who never flew a main, in qualifying order |

- A false start in D-Main and another in B-Main add up to two, because the limit sits on the
  Mains phase.

---

## 7. UI modes (Agreed, detail later)

A mode chooses what the UI shows. It never changes the model underneath.

- **Single pilot mode:** close to what RaceGOW mode shows in grid-rh. Hides locking and
  protests.
- **Simple mode.**
- **Full mode.**
- Later: custom modes, where the director picks what is shown.

Overlays start as a simple integration. Later they become a modular system that people can
pull data from to build their own. The video features of grid-rh's companion should end up in
Grid.

---

## 8. What stays, what is rebuilt

Work happens in this repo on the `primitives` branch, cut from `devel`.

| Area | Plan |
|---|---|
| Build system: `xtask`, CI, release, Tauri, Docker | Keep |
| The log, storage, replay | Keep |
| Adapter interface, clock alignment, dedup | Keep |
| OFT adapter | New, and built first, against `oftd --fake` |
| RotorHazard adapter and plugin | Full rewrite, after OFT. The verified RotorHazard facts in `transport.rs` and `rotorhazard-plugin.html` are kept as the record |
| Velocidrone adapter | Never finished or tested. Rebuilt after RotorHazard; unmerged work is on branch `vd-core-484` |
| Component library and the UI look | Keep |
| Event vocabulary for competition | Replace with §5 |
| Engine: formats, rounds, the six-state heat | Rebuild on the primitives |
| Projections | Rebuild; keep the corrected-passes folds |
| Server: rounds, heats, channels, control | Rebuild; keep auth, streaming, timers, pilots |
| Console screens | Rebuild; take the Run, Tune and Marshal designs from grid-rh |

**Order** (Ryan left this to Claude), with progress as of 2026-11-09:

1. Settle this doc. **Done.**
2. Audit the docs marked under review. **Done**; wholly old docs are in `docs/archive/`.
3. Cut the code down to a skeleton that still builds and passes CI. **Done.**
   `docs/port-list.md` lists what was removed that is worth porting back.
4. Grow the primitives upward from the race, each with tests. **The engine is built**
   (`crates/engine`, pure, about 100 tests):
   - `race`: the race states and checked commands (§5.1).
   - `laps`: laps per seat, with remove, restore, insert, adjust and restart (§5.7).
   - `heat`: a race's finishing order, heat standings, completion rules (§5.4, §5.5).
   - `phase`: the tree, fields, seat references, combined rankings, inherited formats.
   - `presets`: brackets of any shape and stacked mains, checked against §6.1 and against
     96 pilots in mains of 16.
   - `rulings`: false starts, disqualification, locking, protests (§5.7, §5.8).
5. Wire the engine into the Director. **Done** for the server side:
   - `engine::state`: one event's whole state, the one check every command goes through,
     and the facts time alone makes true (a countdown reaching its tone, a timed race
     running out).
   - `server::control`: `GET /events/{id}/state` (one snapshot), `POST …/commands`,
     `POST …/presets/bracket` and `…/presets/mains`, `POST …/passes`, and
     `GET …/changes` (a feed of the log cursor; a console that hears a new cursor reads the
     snapshot again).
   - The console: the event page lists the phases and adds a heat or a bracket, and the Run
     page runs one heat (start, countdown and tone, stop, save or discard). Crossings come
     from a bench button until the console reads seats from a timer.
6. Timers. **The OFT adapter is built:**
   - `adapters::oft`: a Timer's messages translated to Grid's events, and the clock fit.
     Pure, tested with messages from the spec.
   - `server::timers`: the timer directory (`GET`/`POST /timers`, `DELETE /timers/{id}`),
     with each timer's live status.
   - `app::oft`: one connection per OFT timer. It resumes after a dropped link without
     losing or repeating a crossing, logs into the active event, and measures the timer's
     clock so the start tone can be placed on it.
   - Tested in CI against a fake timer inside the test, and by hand against `oftd --fake`.
   - **Signal:** a signal chunk now holds signed levels in the timer's own unit (counts,
     tenths of a dB, or tenths of a dBm), never converted. OFT signal is taken at full rate
     and logged while a race is on, with three seconds either side, in chunks about a
     second long. Signal outside a race is not kept.
   - **Console:** a Timers page (add, remove, live status), and the add-heat and
     add-bracket dialogs pick which timer times the heat. "Bench" stays as a choice with
     no timer, where crossings are logged by hand.
   - Checked by hand on 2026-11-10: a heat run against `oftd --fake` through the Director
     gave laps from the timer, lap 1 timed from the tone, and the race's signal in the log.
   - **Settings and live signal** (server side): `GET`/`PUT /timers/{id}/config` read and
     write the timer's seats, frequencies, threshold and minimum lap. Every write is read
     back, and a write the timer did not keep is reported as refused. `GET
     /timers/{id}/live` sends live signal and each seat's levels for tuning; none of that is
     logged. Checked by hand against `oftd --fake`.
   - **Tune page** in the console: each seat's frequency and on/off, a live signal trace
     per seat with the enter, exit and noise floor levels drawn on it, and the detection
     settings. Changes are a draft until "Apply to timer". Not yet run against a real timer.
   - **Not yet:** mDNS discovery, and showing the logged signal in Marshal.
7. Rewrite each as-built doc (protocol, live state, testing, marshaling) when its area is
   rebuilt, not before. **Started:** `docs/api.md` lists every route as built, and the
   Protocol doc's banner points at it.

**Built in the engine since the first pass:** the run queue (§5.9: the ready heats, the
director's order, and a helper that spaces pilots out), class standings (§5.3), a first-to-N
race that can stop itself, and a time limit for races that are not timed (§5.6).

**Not built yet in the engine:** time and points penalties, and keeping a pilot's channel
between heats (§5.4).

**OFT in tests** (Agreed): the OFT repos will live on GitHub eventually. Until then Grid
uses the local checkout at `~/development/fpv/open-fpv-timer`, and tests that need a fake
Timer run locally only. Anything that depends on the local checkout sits behind a cargo
feature that CI does not enable, so CI on GitHub stays green.

**Not in the first build:** timing with no timer (a manual adapter where the director taps
laps), and seasons. Season rules such as "best N of M events" need measures that §5.5 does
not have yet; they are added when seasons are built.

**Planned, not built: video in marshaling** (Ryan, 2026-11-10). When reviewing laps today,
Ryan opens the OBS recording made through grid-rh's video page and reads the lap times off
the overlay. Grid should do this natively:

- In Marshal, picking a heat opens a small window with that race's recorded video.
- The video follows the marker on the signal trace. Where there is no signal data, it
  follows the race time instead.
- This means grid-rh's video features move into Grid, as already planned.

What the design must leave room for, and does: a race already has its start on the server
clock, and every crossing has a time. A recording only needs one more fact in the log, saying
which race it covers and what server time its first frame is. Then any moment in the race
maps to a moment in the video. Nothing in §5 changes.

**To discuss once the base is done:** whether Grid should control video itself, the way
FPVTrackside does, instead of leaving capture to OBS. This was decided against for grid-rh.
Ryan wants the question opened again for the native app.

**Decided elsewhere and unchanged:**

- grid-rh stays a separate project. It is dropped once Grid does the job.
- The kit always has an OFT Timer. The Director does not act as a timer itself.
- Re-detection is deferred. The path is `oft-core` compiled to WASM in the console.
- Per-seat thresholds go into OFT spec v0.4.

**Old decisions carried forward** (by title; each to be re-checked during the doc revision):
D4 (the console owns the start tone), D9 (the desktop app embeds the Director), D16 (the
RotorHazard plugin), D21 and D22 (runtime clocks, serialized writes), D25 (the removal
record), D26 and D27 (timers are instruments). The rest of the old decision log describes the
round and heat model that this doc replaces.

---

## 9. Questions from the build (all answered)

Six questions came up while building the engine. Ryan answered them on 2026-11-09. Each
answer is now part of the section named, and is built and tested unless it says otherwise.

| Question | Answer | Where |
|---|---|---|
| A disqualified pilot had won a bump-up spot. Who gets it? | The next pilot, by default. The director can keep the spot empty instead | §5.7 |
| Is the first crossing always the hole shot? | It is a race rule. Hole shot is the default; timing from the tone is the other choice | §5.6 |
| Which bracket shapes? | All of them: heats of 2 to 8, single and double elimination, 3 pilots up to 100, big events as mains of 16 | §6 |
| Must a pilot alone in a heat fly? | Director's and pilot's choice: fly it, or the director calls the heat done | §5.4 |
| Does a position override stick? | It is set on a heat and takes effect from there on, until cleared | §5.7 |
| "Ignore crossings before the start" | Comes with the OFT adapter. The filter is built; it needs the tone on the timer's clock | §5.6 |

**Still open:** time and points penalties are not designed in detail yet. They are added
when the Marshal page is built.

---

## 10. Status of the other docs

Nothing was deleted. Each older doc carries a banner at the top. Docs that are wholly old were
moved to `docs/archive/`. Docs under review stay in `docs/` until they are rewritten.

**Old design, kept for history.** These live in `docs/archive/`. Do not build against them.

| Doc | Why it is old |
|---|---|
| `format-model.html` | Three round types as primitives; replaced by §5 and §6 |
| `race-engine.html` | Format generators and the round model |
| `heat-lifecycle.html` | The six-state heat; replaced by the race states in §5.1 |
| `channel-model.html` | Three-scope channel layouts; replaced by seats and profiles |
| `decisions.html` | The old decision log; a new log starts with this doc |
| `event-pipeline.html` and the five `pipeline-*.html` | A fixed pipeline of event stages; replaced by free nesting |
| `roadmap.html` | The old build plan |
| `marshaling-plan.html` | A finished build plan |
| `release-audit-2026-07.html` | An audit of code that is being rebuilt |
| `board-reconciliation-2026-08.md` | A finished working ledger |
| `portable-race-day-2026-11.md` | The proposal this doc grew out of; its vocabulary advice was rejected |

**Under review.** The spine parts still stand. The competition vocabulary is old. Every one
of these was read in full on 2026-11-09, and its banner names the sections that are old.

| Doc | What still stands |
|---|---|
| `vision.html` | The thesis and principles |
| `architecture.html` | The log, the stack, one protocol |
| `timer-adapters.html` | The adapter interface, clocks, passes |
| `live-state-clock.html` | The live fold and the server race clock |
| `protocol.html` | Snapshot plus change stream, auth |
| `marshaling.html` | Lap corrections, signal as evidence, audit |
| `testing-strategy.html` | Golden cases and replay |
| `rotorhazard-plugin.html` | The RotorHazard integration as built |
| `clients.html`, `streaming.html`, `integrations.html` | Surfaces, OBS control, MultiGP and open data |
| `cloud.html` | The push, the warehouse, accounts; its season layer is old |
| `code-conventions.html` | Workspace rules; the crate list will change |
| `mock-race-day.md` | The mock harness; its steps follow the old heat flow |
| `timer-hardware-research-2026-11.md` | The hardware research; its GridNode protocol is replaced by OFT |

**Current:** this doc, and `conventions.html` (how docs are written).
