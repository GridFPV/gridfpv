# Grid primitives

**Status:** revision 9 (2026-11-16): the design to build against, with build progress in §8.
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

**What changed in revision 9** (Ryan, 2026-11-16): §11 is new. It covers the people at a
race who are not the director: spectators and pilots on their own phones, how a pilot signs
in, and pilots signing up for heats. It also sets the rules a remote server must keep. None
of it is built.

**What changed in revision 8** (Ryan, after the first night on real hardware): the minimum
lap and "ignore crossings before" are lap rules of the **event**, set once (§5.1, §5.6). A
timer keeps a separate lap cooldown for bounce (§3.2). A crossing a lap rule left out can be
ruled to count (§5.7). Full mode is built first, shaped like grid-rh's pages (§7).

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
- A timer may keep a **lap cooldown**: after a crossing on a seat it reports no other on
  that seat for a short while (2 seconds by default on OFT). It is there to swallow the
  bounce of one pass at the timer, so Grid is not shown a handful of crossings for one
  trip through the gate. It is a timer setting, saved in the profile. It is **not** the
  minimum lap (§5.1), and it should be shorter than it.
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
- **A timer keeps its identity when its address changes** (Ryan, 2026-11-13). An OFT timer
  has an id of its own. Grid keeps it with the timer, and when the timer shows up on the
  network at a new address (a new DHCP lease), Grid follows it there. It is still the same
  timer: its heats, its profiles and its laps are untouched. The id is an OFT thing; a kind
  of timer without one stays at the address it was given.

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

- **Check-in** to a class roster comes later. When it does, a pilot joins a class roster at
  check-in. (Checking in to an open **seat** during practice is built: §11.9.)
- **Pilots and spectators on their own phones**, and pilots signing up for heats, are in §11.
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
- Lap filters: minimum lap time, and "ignore crossings before" at the start. These two are
  **lap rules of the event** (Agreed, 2026-11-12): set once, on the Event page, and in force
  for every race of the event, flown or still to fly. A format may carry a minimum lap of
  its own for the rare phase that needs one; the nearest wins. Grid applies them to what the
  timer reports. A crossing they leave out stays in the log as "not counted", so nothing is
  lost. Nothing is written to a timer when a race starts.

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
- **Class standings** (Agreed; worked out by Grid since 2026-11-19): each class has the
  phases that count toward its result, in order. The first one decides: for a class with
  mains, the Mains ranking is the class standings. An earlier phase is only **reached back
  to** when the first leaves pilots unplaced or level, for example pilots who never made a
  main are ordered by Qualifying. Practice never counts. This is what gives every class a
  ranking from 1 to n.
  - **Which phases:** the director does not pick them. A phase is **for a class** when, in
    this order: the director said so when making it (a bracket or a round robin made from a
    class's roster keeps the class even if the pilots are then changed by hand); its field
    is that class's roster; it takes its pilots from a phase of that class; the phases
    inside it are for that class (a "Mains" holding heats seeded from qualifying); or the
    phase that holds it is. Every ranked phase that is for the class counts, as one unit:
    Qualifying with its heats is one phase, a bracket with its rounds is one. A phase that
    only names pilots, with no class said, is for no class.
  - **In what order:** the latest phase first, so the last thing raced decides.
  - **What the director can change:** the order (`StandingsSet`), and whether a phase counts
    at all (`PhaseCounts`): a fun race or a warm-up can be left out. A phase added after the
    order was set goes ahead of it.

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
going. A pilot who never finishes means the race never stops by itself, unless the director
marks them **out** (below).

**A time limit** (Agreed) can be set on any race that is not timed: first-to-N or open. The
race stops by itself at the limit, whoever has or has not finished. With both a limit and
the wait above, whichever comes first stops the race. The default is no limit.

Timing from the tone needs the tone's time on the timer's own clock. The same is true of the
filter that ignores crossings before the start. A timer that cannot give that time falls back
to the hole shot and filters nothing at the start. OFT can give it.

### 5.7 Ruling (Agreed)

A ruling is a human decision recorded as a fact in the log. Rulings are never automatic.

- Lap corrections: remove, insert, adjust, split.
- **Count a crossing** (Agreed, 2026-11-12): a crossing left out by a lap rule (too soon
  after the one before, or in the ignored stretch after the start) can be ruled to count.
  From then on no lap rule leaves it out. A lap put in by hand is the same: the lap rules
  are for what a timer reports, never for a person's call. Removing it is the way back.
- **There is one way to remove a lap, and it always behaves the same.** Take laps A, B and C
  in a row. Removing B leaves A as it was, because A was a full lap. C now starts where A
  ended, so B's time becomes part of C. Like a race discard, a removal takes an optional
  reason that is one tap to pick (false detection, incorrect lap, other). The reason is a
  label only and never changes the result. A removal can be undone.
- **Restart point:** the one other thing that can happen to lap B. Marking B as a restart
  means B has no lap time at all, and A and C both stay as they were. A and C can never be
  used together: "best N consecutive" may not span a restart. Always marked by a person.
- **False start:** names the pilot and the race.
- **Out** (Built 2026-11-18, #510): names the pilot and the race. The pilot crashed or will
  not finish. It answers the two open questions this way:
  - "DNF" and "crashed" are **one thing**, called Out. Grid does not ask why.
  - **It changes when the race ends, never the result.** A race that stops by itself stops
    waiting for a pilot who is out. The laps they flew still count, and they are ranked on
    them like anyone else. In a first-to-N race that already puts them below every pilot
    who finished, since they have fewer laps.
  - It can be marked while the race is staging, running or stopped, and taken back
    ("Back in"). Once the race is saved the mark stays as part of the record.
  - **Not built:** ignoring gate crossings after the mark. A quad carried back through the
    gate still counts a lap, and the marshal removes it.
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
| Round robin | R rounds of heats, pilots dealt differently each round; ranked by points summed across every heat |
| Bracket | A chain of rounds where each lineup refers to earlier heats (see below). Every round is a phase that holds its heats, the final too, so each round opens the same way |
| Mains | Brackets stacked with bump-ups (§6.1) |
| Chase the Ace | One heat, raced until a pilot has two wins |
| Single pilot | One heat with one pilot, done when the director says so, best 3 consecutive |
| Season | A ranking rule over several events |

**Built 2026-11-18:** a generated bracket makes every round a phase, even a round with one
heat (it used to put a one-heat round straight in the bracket). The Director keeps a
bracket's rounds latest first, because the ranking stacks them in that order. The console
shows them first round first, the order they are flown.

**Built 2026-11-18:** the console has one Add phase dialog for every kind (Ryan: no row
of different "add" buttons). Its "Kind" is Empty, Bracket, Multi-Main or Round Robin. A new generator is
one more kind there. A phase's own page has Add phase too, for a phase inside it. Mains are
kept best first (A-Main first) and shown bottom main first, the order they are flown.

**Built 2026-11-18: the round robin preset** (#69). It writes one phase holding a phase for
each round, each holding that round's heats. Rules it keeps:

- A round has as few heats as hold the field, the same size or one apart. Round 1 spreads
  the seeds across the heats.
- **A pilot keeps one seat for the whole round robin**, so they never change video channel.
  The price: the pilots who share a seat never meet.
- The later rounds are mixed so that pairs of pilots meet as evenly as they can. The deal is
  worked out from fixed numbers, so the same request always gives the same heats.
- It ranks by the format's points summed across every heat, then wins, then best lap. Each
  round has its own table too.
- Lineups are named pilots, or places of another phase's ranking, so every heat is ready as
  soon as the field is known.

**Decided 2026-11-18:** Chase the Ace stays one heat, raced until a pilot has two wins. It
is not split into several heats in a phase. In a bracket that heat still sits in its own
"Final" phase, like every other round, so the final is never a heat on its own.

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

**Full mode is built first** (Ryan, 2026-11-12). The other modes come once a single pilot
can run a home series in full mode. Full mode takes its shape from grid-rh, one page per
job. The pages are on two sides with a switch between them (Ryan, 2026-11-13). **Setup** is
what is kept for every event: Events, Timers, Seats, Pilots, Classes, Formats, Video. **Event** is
the open event: Details, Phases, Run, Marshal, Results, Overlays. Each side remembers its
page, and Help is on both. What it keeps from grid-rh: lap rules set once on the
event, seat profiles, the format list, the Run and Marshal pages, tuning on a recording,
and built-in help. What it leaves behind: teams (individual is the norm), archives, and
RotorHazard's shape of stages, heats and rounds.

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
   - The console: the event page (phases, "Up next", classes with rosters and standings,
     rulings on heats and phases), the Run page (start, countdown and tone, stop, save or
     discard), and the Marshal page (a to-do list, lap rulings, the race's signal trace,
     protests). A heat is timed by a timer, or by the bench, where crossings are a button.
   - The race overlay (`#/overlay/race`, a page for an OBS browser source) is the panel
     grid-rh's race overlay drew, on Grid's data: the same sizes, the run's name as the
     title, the hole shot, pilot colours, and two logos. Where it sits (corner, offsets,
     size, background) is one look for the whole Director, set on the Overlays page and
     used live, in the replay and in every redrawn video.
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
   - **OFT protocol v0.4, server side:** a seat can carry its own threshold on a timer
     that says it takes them, and OFT timers are found on the network (`GET
     /timers/discovered`). The Tune page has a seat's own levels and the Timers page has
     "Found on the network". Both were checked in a browser against the real daemon.
   - **OFT changes of 2026-11-12:** the exit hold (how long the signal must stay below the
     exit level before a crossing ends) is read, shown on the Tune page and written back.
     Before this, every "Apply to timer" put it back to the timer's default. A timer that
     can keep its settings across a restart is asked to keep them after every change; this
     part is tested against a fake only, because `oftd` cannot keep settings. The timer's
     network address and restart are left to the timer's own page.
   - **Minimum lap and lap cooldown (2026-11-12, Ryan):** the minimum lap is a lap rule of
     the event (§5.1), applied by Grid. The timer's own `min_lap_ms` is treated as a lap
     cooldown for bounce: a timer setting on the Seats page, 2 seconds by default, saved in
     the seat profile. Grid no longer writes to a timer when a race starts. (For one
     evening Grid set the timer's minimum lap to the format's at every start; that is
     gone.)
   - **OFT spec v0.5 (2026-11-12):** `seats_per_node` is gone from the protocol; the timer
     decides how seats are spread over its receivers. Grid no longer reads, shows or sends
     it. A v0.4 timer, which needs the field in a write, refuses "Apply to timer" until its
     firmware is updated.
   - **A timer that restarts mid-race:** its clock begins again. The laps either side are
     kept, and the stretch across the restart has no time.
   - **Tested against the real daemon** (`crates/app/tests/oft_live.rs`, local only):
     settings, races, the race's signal, a restart between races and one mid-race.
7. Rewrite each as-built doc (protocol, live state, testing, marshaling) when its area is
   rebuilt, not before. **Started:** `docs/api.md` lists every route as built, and the
   Protocol doc's banner points at it.

8. Full mode in the console (2026-11-12). **Built**, one page per job, on two sides
   (Setup and Event) with a switch between them:
   - **Details** (the event's first page): details with Edit (`PUT /events/{id}`), the
     event's lap rules, classes.
   - **Seats:** one timer's seats, live signal and timer settings, as a draft that is
     applied and read back. Seat **profiles** (`GET`/`POST /timers/{id}/profiles`, apply
     writes to the timer). **Tune** on a seat records its signal and sets the enter and
     exit levels on the recording; the passes shown follow the timer's own rule with its
     exit hold. A recording lives in the page only.
   - **Timers:** the directory, with timers found on the network.
   - **Pilots** and **Classes:** the lists that outlive events.
   - **Phases:** a short list (one line a phase, with where it stands and the heat on the
     timer), then Up next, then heats that stand alone; and Add phase, Add heat, Add
     bracket. A line opens **the phase's own page**: its own heats and lineups, its ranking
     and rulings, and its sign-up sheet. A phase inside a phase (a bracket's round) is not
     shown there in full: the page lists the phases inside as lines, and each has a page
     of its own. On the list, a phase that holds phases unfolds to show them. A phase or
     heat is given a saved format; a heat under a phase can use the phase's. (Reworked
     2026-11-17: the page used to be one long tree with every sign-up sheet stacked above
     it.)
   - **Formats:** the saved formats (`/formats`) as a table, with an editor over the same
     rules-as-data the engine reads (§5.5). A new Director starts with four.
   - **Run:** a phase, then one of its heats, on the timer. Keyboard shortcuts, lap
     callouts, and the best run of N laps on a seat card.
   - **Marshal:** opens on the race that most needs a look; the race list shows one phase
     at a time and leaves discarded races out unless asked.
   - **Results:** class standings and each phase's ranking with the numbers behind it.
   - **Help:** one page per console page, Markdown in `docs/help/`, bundled into the
     console so it works with no internet.

**Built in the engine since the first pass:** the run queue (§5.9: the ready heats, the
director's order, and a helper that spaces pilots out), class standings (§5.3), a first-to-N
race that can stop itself, and a time limit for races that are not timed (§5.6).

**Also built:** a pilot keeps their seat, and so their channel, from one heat to the next
(§5.4). Only seats filled from another phase are moved; when two pilots last flew the same
seat, the higher seed keeps it. A director's override of the result is not built.

**Not built yet in the engine:** time and points penalties.

**Known loose ends** (as of 2026-11-10; none blocks a race):

- The overlay, the Run page and the Results page work out "best N consecutive laps"
  themselves (one shared helper), as the engine also does. The Director should serve it, so
  the rule lives in one place.
- A phase's format cannot be changed in the console once it is added; the phase is removed
  and added again. A phase cannot be renamed or moved.
- Tune recordings live in the page. §5.10 says they belong to the timer and are stored
  with it; that is not built.
- The event's lap rules apply to races under a locked phase too. Changing them after a
  lock changes an official result without an unlock.
- A race has no "saved at" time in the snapshot, so the overlay infers how long to keep a
  result on screen.
- The trace of a race in which the timer restarted is drawn on two clocks at once. The lap
  list is right; the picture is not.
- A class cannot be removed from an event in the console. A timer can be removed while a
  heat still uses it.
- Every unlocked phase row has a Lock button, which is a lot on a big bracket.
- The timer list and the active event are polled, not pushed. So are the saved formats
  and the seat profiles: a second console sees a change on its next read.
- Refusing a start with no countdown on a timer that needs warning is not enforced.
- The desktop app is not compiled or run by any gate on this machine.

**OFT in tests** (Agreed): the OFT repos will live on GitHub eventually. Until then Grid
uses the local checkout at `~/development/fpv/open-fpv-timer`, and tests that need a fake
Timer run locally only. Anything that depends on the local checkout sits behind a cargo
feature that CI does not enable, so CI on GitHub stays green.

**Not in the first build:** timing with no timer (a manual adapter where the director taps
laps), and seasons. Season rules such as "best N of M events" need measures that §5.5 does
not have yet; they are added when seasons are built.

**Video in marshaling: built** (asked for by Ryan on 2026-11-10, released in v0.5.1; the
plan is kept here as written). When reviewing laps today,
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

**Video, step 1: built** (Ryan, 2026-11-15: "it should be built in to Grid itself, no need
for a separate app"). grid-rh's video companion is being ported into the Director, not
kept as a second program. The Director talks to OBS itself and has it record each race:
started at the countdown, stopped a few seconds after the stop, named after the race,
moved to a trash folder on a discard. The fact described above is in the log
(`VideoRecorded`: the file, and the Director's clock at its first frame). Settings and
status are on the Video page under Setup, and the Run page says before Start if a race
would not be recorded. Grid is run as the native app on the OBS machine, so moving files is
the normal case.

**Video, step 2: built** (Ryan, 2026-11-15: "do redrawing and then the source record now
together"). Two things, which need each other:

- **A scene of its own.** Instead of OBS's own recording, the Director can record one
  scene through the Source Record plugin: the clean scene, with the camera and no overlay.
  A second scene with the live overlay on it can be recorded alongside. OBS's own recording
  and stream are left alone.
- **Drawing the overlay afterwards.** The overlay has a replay page, which shows a
  finished race as it stood at any moment of its recording. The Director opens that page in
  a browser with no window, takes a picture of it for every frame, and has ffmpeg lay the
  pictures over the clean recording. So a lap fixed in Marshal shows in the video: fix the
  lap, draw again.

Two more facts carry this. `VideoRecorded` says whether the file is clean, where the
overlay scene's file is, and a mark of the laps that one was filmed with. `VideoRendered`
names a drawn video and the mark of the laps it shows. The mark is worked out from the
race's laps, so the snapshot can say for every race whether its overlaid video still
matches: filmed, drawn, none yet, or laps changed. Neither fact changes a result.

The Marshal page plays a race's video lined up with its clock: a line on each signal trace
shows where the video is, and a click on a trace moves the video there.

ffmpeg is not shipped with Grid and there is no installer to fetch it. The Video page has a
**Get ffmpeg** button: the Director downloads a ready-made build with the system's `curl`
and `tar` and keeps it in `tools` in the data folder. This keeps the desktop app one
portable file with its data beside it.

**Moving to another computer: built** (Ryan, 2026-11-15: "an easy way to copy or export all
our grid data so we can move it between computers"). The Data page saves one zip of
everything that is not tied to a computer: pilots, classes, formats, seat profiles, timers,
the open event and every event's log. The video setup stays behind (it holds the OBS
password and paths on that machine), and so do recordings. Bringing a copy in **replaces**
the data; it does not merge. The Director only checks and keeps the file. The swap happens
at the next start, before any file is open, and what was there is moved to a `backups`
folder. Nothing is deleted.

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
| `timer-hardware-research-2026-11.md` | Research toward a timer of Grid's own. Grid has none: timers and their protocol are the OFT project |

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

**Current:** this doc, and `conventions.html` (how docs are written).

---

## 11. People at the race: spectators, pilots and sign-up (Agreed, not built)

Agreed with Ryan on 2026-11-16, over one long discussion. Nothing here is built. The
GitHub issues are #14 (the page), #432 (who may do what) and #16 (the remote server).

Until now only the director used Grid. This section adds the other people at a race.

**Built so far** (2026-11-16):

- **Step 1, letting other devices in safely.** The desktop app answered only its own
  computer, and anything that could reach the Director could run the race. Now a switch on
  the Network page (off by default) starts a second listener for other devices. A device
  that is not the Grid computer is a **visitor**: it can read the public pages and nothing
  else. A director on another device signs in with the access token. The rules and the
  routes are in `docs/api.md` ("Who may do what").
- **Step 2, the spectator's page** (§11.2). A visitor's browser shows the open event: the
  race that is on with a running clock and each lap as it is flown, or how the last race
  ended; the heats coming up with who is in them; and the results. It updates by itself and
  follows the director to another event. It reads the console's own snapshot through
  `/public/event…`, with the director's things taken out (video paths, notes, protests, a
  pilot's real name and outside ids, a timer's address).
- **Step 3, a pilot signs in** (§11.3). Each pilot in an event has a six-character code.
  The director opens the **Pilot sign-in** card on the event's Details page and shows a
  pilot theirs, as letters or as a QR code. The pilot types it on their phone, or scans
  it, and the page then knows them by name. One device at a time; one event; the director
  can give a new code or sign a phone out.
- **Step 4, what a pilot sees** (§11.2). A signed-in pilot gets a **Me** tab: their heats
  in the order they will fly, how many heats away each is, the seat and, for a seat on a
  timer, its channel; and their own races lap by lap with the best lap marked. An alert
  sits above every tab when they are two heats away or closer, next, or flying now, and
  the phone buzzes each time it gets closer. It only reaches a phone with the page open.
  Everyone, pilot or not, sees each seat's channel in the heats coming up. A pilot's row
  is marked in the race, the heats and the results.
- **Step 5, sign-up** (§11.5). A phase that holds heats can be set for pilots to sign up.
  The director turns it on from the Phases page and gets a sheet: heats down, seats across.
  Pilots take a free seat, leave theirs, or take the next free spot on a seat, from a
  **Sign up** tab on their phones. Heats are plain heats under the phase, numbered as they
  appear; at most N that are open and not full exist at once, and the next appears when one
  fills, is started, or is closed. The limits (spots held, runs, rest) bind pilots; the
  director is bound only by what cannot be otherwise. The director adds, drops (counted or
  not), pulls forward on the same seat, closes a heat to pilots, and gives runs to everyone
  or to one pilot.

  Where the build differs from the text below, and why:

  - **"On the timer."** As §11.5 says, a heat locks to pilots when the director puts it on
    the timer, and the director can add a pilot up to the start. (The first build could
    not do this, because the heat on the timer was kept only in the console's address
    bar. Since step 8 the Director keeps it.) Taking a heat off the timer without flying
    it opens it to pilots again. The **Close to pilots** button is still there, for
    closing a heat that is not on the timer.
  - **A drop after the race.** A pilot in a saved race has used a run. To give it back,
    the director gives that pilot one more run, since a flown heat cannot be changed.
  - **Not built:** opening and closing sign-up on a set time.
- **Step 6, the kiosk** (§11.4). A tablet at the race director's stand, opened at
  `#/kiosk`. **Only the director makes a tablet a kiosk**: the Network page makes a
  **kiosk code**, the director types it on the tablet, and the code is used up. One code,
  one tablet. The tablet is then listed on the Network page until the director removes
  it. A pilot's phone cannot become a kiosk.

  With nobody at it a kiosk shows a code box and nothing else. A pilot types their own
  code (or a handheld scanner types the link in their QR code) and gets the sign-up sheet
  as a large grid for a tablet on its side. They press **Done** when finished. The page
  signs them out by itself after 45 seconds with no touch, counting down for the last 15.
  A reload signs the pilot out too; the tablet stays a kiosk.

  A code typed at a kiosk does **not** count as the pilot's one device, so it never signs
  their phone out. The Director gives the kiosk a **pass** that lives in memory only,
  works for that pilot and event, only on a tablet that is still a kiosk, and stops 2
  minutes after it was last used, or when the director gives the pilot a new code or
  signs them out.

  - Kiosks come in on the same listener as phones. What keeps a phone from being a kiosk
    is the kiosk code, not the port (agreed with Ryan, 2026-11-17).
  - **Not built: the tablet's camera reading a QR code.** A browser only gives a page the
    camera over HTTPS or on the computer itself, and Grid at the field is plain HTTP on
    the local network. A cheap handheld scanner that types what it reads works today.
- **Step 7, the pages after the first try-out** (Ryan, 2026-11-17).
  - **Spectators** get the race, the heats coming up and the results. They do not see
    sign-up at all.
  - **Pilots** get the same three, plus **Me** and **Sign up**. The way in is a **Pilot**
    link in the top corner; signed in, their callsign and **Sign out** are there.
  - **Results** are picked phase first, then **Overall** or one heat. A heat shows each
    of its races, and a race opens to its lap times in a large format. So does a race on
    the pilot's own tab.
  - **One race tab, always called Current race** (Ryan, 2026-11-17). It shows **the heat
    on the timer**, always with a clock: who is in it before it is flown, its race while
    that is on, and its result once it is saved. The result stays until the director puts
    another heat on the timer.
  - **A seat is named with its channel wherever it is named**: "Seat 2 · R2". The channel
    is what a pilot needs. It comes from the timer, so a bench seat has none. The console
    and the visitor's page read it the same way, into the one name resolver.
  - **The sign-up sheet a pilot sees lists only heats still open.** A heat that is closed,
    on or flown is off it. A full open heat stays, since someone may drop out. The
    director's sheet shows every heat.
  - **Kiosks can be named** on the Network page ("By the gate"), and a kiosk shows its
    name while it waits. Kiosks stay on the same listener as phones (agreed).
  - **The director's sign-in** from another device is at `#/director`. Nothing on the
    visitor's page links to it. The director is expected at the Grid computer.
- **Step 8, the heat on the timer is the Director's** (Ryan, 2026-11-17: "I have to set
  the heat that is on the timer. That is always the heat I am referring to when I say the
  active race. You absolutely should know about it").
  - **On the timer** is one heat: the one the director is running now, whether its race
    has started or not. The director puts a heat there from the Run page or with **Run**
    on a heat. Starting a race puts its heat there too. It stays after the race is saved,
    until another heat is put on.
  - It is a fact in the event's log (`HeatOnTimer`), so every console, the spectators'
    page, the pilots' alerts and sign-up all read the same heat, and it is kept when Grid
    is closed and opened.
  - While a race is waiting to be saved or thrown out, no other heat can go on the timer.
  - **"The current race" means the heat on the timer, everywhere.**
  - Before the director has put anything on the timer, the Run page shows the heat that
    would be run first and tells the Director so.
  - The kiosk shows one sign-up sheet at a time, with a picker when a pilot is in more
    than one, and only the sheets that pilot is in.

### 11.1 Two parts, and the rule between them

- **Local:** people at the race use their own phones, on the field's network, served by
  the Director. No internet is needed.
- **Remote:** a server away from the field shows the same things and keeps long-term
  records. It is covered in §11.7, as rules only.

**The rule:** race day never needs the remote part. Grid must never rely on any one hosted
service. Everything in §11.2 to §11.6 works with the laptop alone.

### 11.2 Two kinds of visitor

| Visitor | Signs in? | Can do |
|---|---|---|
| **Spectator** | No | Look only |
| **Pilot** | Yes (§11.3) | Everything a spectator can, plus their own things |

**A spectator sees:** the race that is on, results and standings, and the heats coming up
with who is in them.

**A pilot also sees:** their own upcoming heats, the channel for each one, the sign-up
(§11.5), and their own race data laid out more cleanly.

**Alerts** for a pilot: "you are 2 heats out", "you are next". With no internet these can
only reach a phone that has the page open (a sound, a buzz, a banner). Alerts to a locked
phone go through Apple's and Google's services, which need internet, so those belong to the
remote part.

The director's console is not part of this. It keeps its own access rules.

### 11.3 Who a pilot is, at the race

- **The Director's pilot list is who people are at a race.** It works with no internet.
- **The director adds every pilot,** by hand or by import (from MultiGP, for example). A
  pilot never adds themselves.
- **Signing in:** each pilot has a short **code** for the event. The director shows it
  from the console as a **QR code** and the pilot scans it, or the pilot types the code.
  The QR code is only the code in a form a camera can read.
- **It lasts for that event only.**
- **One device.** If a pilot signs in on a second phone, the first one is signed out.
- A pilot who is signed in through a remote server account does not need the event sign-in.

There are two QR codes, for two jobs:

| Code | Who it is for | What it does |
|---|---|---|
| **Event code** | Anyone | Opens the page as a spectator. It can be printed or put on a screen |
| **Pilot code** | One pilot | Signs that pilot in. The director shows it from the console |

### 11.4 The kiosk

A **kiosk** is a tablet the director sets up at the race director's stand, for a pilot
with no phone or a flat battery. It must exist. Only the director can make one, with a
kiosk code that works once (Ryan, 2026-11-17).

A pilot does **not** just tap their name. To act at the kiosk they prove who they are with
their code (§11.3): they type it, or hold up its QR code, on a phone or printed. There is
no separate PIN.

### 11.5 Sign-up

A phase can be set so that **pilots put themselves into heats**. This is for practice and
qualifying, in general.

**Sign-up is never required.** A practice with open seats (§5.4) always works: the timer
runs, pilots go up when they like and sort it out between themselves, and lap times are
called out by seat or channel, not by name.

**The rules:**

- **Where it is set:** on the phase.
- **Who can sign up:** the pilots registered for that phase. Where a phase serves two
  classes, pilots from both can share a heat.
- **When:** the director opens sign-up and closes it. The director can also set a time
  for it to open and a time for it to close, so it happens by itself.
- **Heats:** each has its number from the moment it appears, and heats fly in number
  order. So a pilot who signed up for heat 7 knows heat 6 flies first.
- **How many are open:** at most **N** heats that are not full are open at once. The
  director sets N. When one fills, the next one opens.

  Say N is 3. Heats 1, 2 and 3 open. Heat 1 gets one pilot, heat 2 fills, heat 3 gets
  three. Heat 4 opens, because only heats 1 and 3 are not full. When heat 4 fills, heat 5
  opens.
- **A heat that is not full can still fly.** It is the director's job to see that no heat
  flies with one pilot. The tool for that is the **pull** (below).
- **Taking a spot:** a pilot picks a heat and a free seat. Any free seat: Grid does not
  give pilots a seat. A seat is a channel, and it is the same channel for the whole phase.
  There is also **next free** on a seat, which takes the first open spot on that seat.
- **More than one spot:** a pilot can hold spots in several heats, even two in a row.

**Limits**, all set by the director:

| Limit | What it caps | Default |
|---|---|---|
| **Open heats (N)** | How many heats that are not full are open at once | 3 |
| **Spots held at once** | How many spots a pilot can hold that are not flown yet | To be chosen |
| **Total runs** | How many heats a pilot can fly in the phase | To be chosen |
| **Rest between heats** | Heats a pilot must sit out between two of theirs | Off |

The director raises a limit for **everyone at once**. That is the normal case ("one more
run each"). The director can also change it for **one pilot**, at any time.

**What uses up a run:** a race that is saved. A race that is discarded or flown again does
not.

**Leaving and dropping:**

- A pilot can leave a spot until the director puts the heat on the timer.
- From then on the heat is **locked** to pilots. Only the director can change it. The
  director can still add a pilot up to the moment of Start.
- The director can **drop** a pilot from a heat, and chooses whether it counts as a run.
  Not counting it is for a pilot who never got to fly, or had video trouble.

**The pull:** when a heat is thin, the director can pull a pilot forward from a later
heat. A pilot is only pulled onto **the same seat** they signed up for, so their channel
does not change. The pilot gets an alert. Grid does not ask them: the director asks in
person first.

**Three ways in:** the pilot's own phone, the kiosk, and the director's console.

**Results:** every run a pilot flies in the phase feeds the phase's ranking under its
format (§5.5, §5.6). Sign-up adds no new ranking rule.

### 11.6 What is taken from ZippyQ, and what is not

MultiGP's ZippyQ does a similar job and was looked at for ideas. Its rules are a good
start. Ryan's view of its screens: "a pain and not user friendly". So Grid takes rules, not
screens.

- **Taken:** a limit on spots held at once beside the total; one button for the next free
  spot; a kiosk; the run count shown beside each name; moving pilots up to fill a thin
  heat.
- **Different:** ZippyQ is one line per channel, and a pilot gets the next slot in it. In
  Grid a pilot picks a heat and a seat, with N heats open, so friends can fly together.

### 11.7 The remote server: rules only

The server is not designed yet. These rules are agreed, and anything built must keep them.

- **No reliance.** Grid never depends on any one hosted service. Race day works without
  the server (§11.1).
- **Self-hosted and hosted.** Anyone can run their own server. A hosted one may also be
  offered.
- **The open-source version always has every feature.** A hosted service may keep some
  things private, but never a feature.
- **Accounts belong to a server.** No single account for the whole world is needed.
- **If there is ever one central Grid service,** accounts on it must be free, and it is
  the most trusted source of who a pilot is.
- **Matching a pilot to an account:** a pilot has an id on each server and gives it to the
  director, who puts it on the pilot. It is one more outside id (§4).
- **A pilot can link their accounts** on two servers. Both servers must agree to the link,
  and servers need stronger sign-in than the event code. Once linked, the pilot can see
  their data from either server.
- **The server shows what the local page shows**, plus long-term records. So the page and
  its reads should be one thing, served from two places.
- **Video** on the server comes later.

### 11.8 Order of work, and what is not decided

**Local comes first** (Ryan, 2026-11-16). The remote part is second: "once we figure out
local, then remote becomes easier." Long-term records come last.

One case shows what remote is for. Ryan runs RaceGOW on a laptop in the garage and wants to
see the results on his desktop upstairs while editing. A computer on the same home network
can already do that by opening the local page. A server is for when the two are not on the
same network, or the laptop is switched off.

**Not decided yet:**

- The defaults for spots held at once and total runs.
- What the server stores, and how a Director sends it when there is internet. Future work.
- Which long-term records come first: results per pilot, seasons, records per track.

One thing to check in the code before sign-up is built: lap callouts for an open seat
should say the seat or channel. Today a callout is written for a named pilot.

### 11.9 Check-in to an open seat (Built 2026-11-18, #399)

This is for practice run as **one long heat**. The heat has open seats (§5.4), the director
starts its race, and the timer just runs. Sign-up (§11.5) is for practice planned as heats.
Check-in is for practice with no plan at all.

- **Taking a seat.** A pilot who wants to fly takes an open seat. Only a seat with no pilot
  named on it can be taken, and only while the race is counting down or running.
- **Whose laps.** The pilot's **first pass after taking the seat starts their first lap**.
  The lap that was under way when they took it is not theirs. This is decided by the order
  of the log, never by a clock.
- **When it ends.** A pilot keeps the seat until someone else takes it, they drop it, or the
  director drops them. Their laps stay theirs. One spell on a seat is a **stint**.
- **One seat at a time.** A pilot who takes a second seat leaves the first.
- **Idle drop.** A format can set "drop an idle seat after N seconds". A seat with no pass
  for that long is dropped by the Director. The wait starts again at each pass. Left empty,
  nobody is dropped.
- **Practice laps are the pilot's, ranked or not.** They show on the pilot's own page under
  "My practice", live while they fly. They do **not** enter the heat's ranking: an open
  seat still ranks nobody.
- **Three ways in,** like sign-up: the pilot's phone, the kiosk, and the director's Run
  page. A pilot can only ever take or drop a seat for themselves.

**Not built:** counting a checked-in pilot in the heat's ranking; a pilot marking themselves
"out" (a checked-in pilot drops the seat instead); check-in to a class roster (§4), which is
a different thing with the same name.

### 11.10 Three ways in: the Grid computer, Public and Remote RD (Built 2026-11-18)

Grid is reached three ways. Each is a listener on its own port, and the way a request came
in decides what it may do.

| Way in | For | Port | Switch |
|---|---|---|---|
| **The Grid computer** | The director, sitting at it. No password | 8080, this computer only | Always on |
| **Public** | Spectators, pilots, kiosks (§11.2 to §11.5) | 80, or 8081 when 80 cannot be used | **Enable Public** |
| **Remote RD** | The director, on another device. The whole console | 8082 | **Enable Remote RD** |

The rules:

- **The Grid computer's own listener is never put on the network.** Whoever reaches it is
  trusted as the director, so it only ever listens on this computer. There is no setting,
  flag or start-up option that changes this. A Director on a machine with no screen is
  reached through an SSH tunnel, which arrives as the machine itself.
- **Nobody is the director at the Public door.** A token or a director's sign-in sent
  there is not looked at, and the director's sign-in does not answer there. This holds even
  for a browser that is signed in at the Remote RD door: a browser sends its cookies to
  every port of an address, and at the Public door they count for nothing.
- **The Remote RD door is not for looking.** Until the access token is shown it gives the
  sign-in page and nothing about the race, not even the event's name.
- **Nobody who comes in by a door is local,** even from the Grid computer itself.
- **Both doors are off until switched on,** each by its own switch on the Network page.
  Switching one off closes every connection it had open.
- **Ports.** Each door has a usual port and the director can pick another. Public tries 80
  first, so the address is short to type and to put in a QR code; where the computer does
  not allow that, it uses 8081 and says so. The two doors and the Grid computer's own
  listener never share a port.

**The access token** is what the Remote RD door asks for.

- Grid makes it the first time the door is opened: twelve letters and digits, in three
  groups, with none that look alike. The director can type their own instead.
- It is **shown on the Grid computer**, at any time. A device that signed in with it is
  never told it, and cannot change it.
- Changing it, or shutting the door, signs every other device out at once and closes what
  they had open.
- It is kept in `network.json`, which stays out of a copy made on the Data page.

A kiosk, a pilot's QR code and the spectator's page all use the **Public** address.

**The kiosk's QR code** (built 2026-11-18). While a kiosk waits for a pilot it shows a QR
code that opens the public pages on a pilot's own phone. It holds an address and nothing
else. It is drawn in Grid's colours, not black and white: Grid's dark ground for the
squares, a pale tint of Grid's green behind them. The squares are always the dark ones, so
every phone camera reads it. The address is the one the kiosk itself opened Grid by.

**The public address** (built 2026-11-18). The director types the address people should
use to reach the public pages, on the Network page. Every address Grid gives out for the
public pages, and every QR code (a pilot's, the printed sheet, the kiosk's), then uses it
and not a number. It is a whole address, with `http` or `https`, and nothing but a name and
a port: no path and nothing after `?` or `#`, so nothing can ride along in a QR code. It
covers two cases:

- **A name on the race network**, where the field's network has DNS: `http://grid.race.lan`.
- **An address on the internet.** The Director runs at the track on a hotspot or a
  satellite link, and a tunnel (Cloudflare Tunnel, or the like) carries an internet address
  such as `https://race.example.org` to the Public door. People then need not be on the
  field's network at all.

Grid does not run DNS or a tunnel itself; it only uses the address it is told. Only the
**Public** door is ever put behind a tunnel: the Remote RD door and the Grid computer's own
listener stay on the field's network.

**Agreed, not built.** Each of these is decided (Ryan, 2026-11-18) and waits its turn.

- **HTTPS.** Three ways, built in this order: the director's own key and certificate; a
  certificate Grid signs itself (every phone warns about it, so it suits Remote RD more
  than Public); and Certbot, as an option only, since it needs the internet and race day
  must not (§11.1). With HTTPS on, Public's usual port is 443.
- **Overlays for an OBS on a second computer.** Today the overlay pages are served by the
  Grid computer's own listener, so OBS must run on the Grid computer. The plan is to serve
  them at the Public door, since an overlay shows only what a spectator may see, and to
  keep the overlay settings for the director. To check first: that the overlay pages read
  nothing but the public routes.

## 12. Setup is set once (Agreed 2026-11-18)

**The rule:** Setup is what a director sets before the event and then leaves alone. During
an event the only Setup page they should need is a timer's seats. Anything that belongs to
one event is on the event's side.

What that changed (built 2026-11-18):

- **Overlays** is a Setup page. What an overlay shows is the open event, but where it
  sits, its logos and its URLs are the same for every event.
- **Video** in Setup is only how OBS is set up. What was recorded, and the overlay runs,
  are on the event's **Recordings** page.
- **Seats** puts the timer's own settings first, and each seat's signal inside that seat's
  row, with its settings.
- **Formats** is laid out like Classes: one row each.
- **Classes:** Grid still ships with the standard classes, and they are **not special**:
  each can be edited, hidden or removed like a class the director made. The ones removed
  can all be put back, as shipped. The tag beside a class is its **org**: free text, or
  empty. There is no fixed list of orgs, no "Custom" tag and no "Built-in" mark.
- **Data** is out of the menu. Grid is portable, so "where the data is" is not shown. The
  page still works at its address, and the Director's routes are unchanged.

**The event's pages** got the same pass (built 2026-11-18):

- **Details** says less. Each class is a block that is shut until opened; inside is its
  roster, changed in place.
- **Pilot codes** is a page of its own among the event's pages: the sign-in codes (§11.3),
  and a sheet of them to print, four or six to a page, with each pilot's callsign, QR code
  and code. The browser prints it, or saves it as a PDF.

### 12.1 More than one timer (Agreed in outline, not designed, not built)

Today a heat's seats are on one timer, and the Seats page shows one timer at a time, with
a picker when there are several. Two things are wanted, and they are different:

- **Primary and backups.** Two or more timers at the **same gate**, the start/finish. One
  is the primary. If it misses a pass, or fails, a backup's pass stands in.
- **Splits.** Timers at **other gates** round the course. A pass there is not a lap. It
  gives split times, and shows a pilot's progress during a lap.

Open questions, to settle before any of it is built:

1. **Which pass counts** when the primary and a backup both saw it, and when only the
   backup did. How close in time two passes must be to be the same pass. Whether the
   marshal sees the backup's passes and can take one by hand.
2. **Failover.** Whether Grid switches to a backup by itself when the primary drops, or
   the director does it.
3. **What a split is** in the model: a pass (§2) that is not a lap. Where it is kept, how a
   lap's splits are shown, and whether any ranking rule may use them.
4. **Seats.** One seat is one pilot's channel. With several timers a seat is on each of
   them. Whether the channel is set once and written to every timer, or per timer.
5. **The Seats page** with several timers: one page per timer as now, or one row per seat
   with a column per timer.

### 12.2 Saved OBS profiles (Wanted, not designed)

A named set of video settings the director can switch between, as a seat profile is for a
timer. To decide: whether a profile holds only where OBS is (address and password), or also
what records, the scenes and where the files go. The password is never shown again once
stored (§ security), so a profile can be switched to but not read back.

### 12.3 The Run page's seat cards with many laps (Agreed 2026-11-18, not built)

A seat's card lists every lap, so in a long open practice it grows without end. It is to be
done with the pass over the event's pages.

- **The card shows the last 9 laps**, newest last. Its height is then fixed. With 9 laps or
  fewer nothing is hidden, so a short race looks as it does now.
- **The best lap is always on the card.** When it is older than the last 9 it is shown as
  one more chip at the front, marked best. In a heat ranked on the best N laps in a row,
  that run is kept on the card the same way.
- **In open practice the laps are the current pilot's:** the last 9 of whoever holds the
  seat now (§11.9), not the last 9 on the seat.
- **"All laps", under the cards, shut until opened.** One table for every seat: lap, seat,
  pilot, time, and in practice which stint. It can be narrowed by seat and by pilot, and
  sorted by time.
- **The Marshal page still shows every lap.** That is where laps are changed.


### 12.6 The pass window is an event lap rule (Agreed 2026-11-19, built)

Some tracks fly close by the gate just before going through it. The timer sees two
crossings. OFT spec v0.6 gives the Timer a **pass window** (`pass_window_ms`, 0 to 10000,
0 is off): crossings that start within the window are one pass, and the strongest is
reported.

- **It is set on the event, and only there.** `LapRules.pass_window_micros`, beside the
  minimum lap. The Seats page does not show it. A track needs it or does not, and the
  track belongs to the event.
- **The timer applies it.** Grid writes the open event's value to every connected timer
  that says it has one (`capabilities.config.pass_window_ms`), and reads it back. It does
  so when the lap rules change, a heat goes on the timer, a race starts, another event is
  opened, and a timer connects.
- **Nothing else on the timer is touched.** The write sends back what was read, with the
  window changed. A settings write from the Seats page carries the timer's window over,
  because a write without it would switch it off.
- **A timer that cannot is said, not hidden.** With a window on the event, the Run page
  says a timer with none does not have it in force. A write that fails shows the reason.
- **It does not change laps already recorded.** Unlike the minimum lap, it works before
  Grid sees the crossings.
- **Later, not built:** Grid merging close crossings itself, so the window holds on every
  kind of timer and can be changed after the race.
