# Phases

The Phases page holds everything that is raced at the event.

It is a short page. From the top:

- **Phases**: one line for each phase, with where it stands, its format, and how many of
  its heats are flown. Click a line to open that phase. The line also says which heat is
  **on the timer**, and whether pilots can sign up.
- **Up next**: the order the heats will be run in.
- **Heats on their own**: heats that are not under any phase.

A phase's heats, lineups, ranking and sign-up sheet are on the phase's own page.
**← All phases** takes you back.

A phase can hold other phases. A bracket holds its rounds, for one. Its line says
"2 phases inside" and has a small arrow button on the left. Press the arrow to show the
phases inside, each on a line of its own. Press it again to hide them. Click any line to
open that phase.

A **heat** is a lineup of pilots on seats. It can be raced once or many times. A **phase**
is a part of the event that takes pilots in and gives a ranking out. Practice, Qualifying
and a bracket are phases. A phase can hold heats, and it can hold other phases.

## Adding things

- **Add heat**: one heat. Give it a name, pick what times it, pick the pilots in seat
  order, and pick a format. The first pilot gets Seat 1. **Start from a class** fills in a
  class's roster. A heat can sit at the top of the event or **under** a phase.
- **Add phase**: a phase. **Kind** picks what you start with:
  - **Empty**: a phase with no heats yet. Pick its format once and every heat under it
    uses it. **For the class** makes the class's roster the phase's field, so every pilot
    in the class gets a place. Then add heats with **+ Heat** on the phase's line.
  - **Bracket**: every heat of an elimination bracket at once. Pick the pilots in seed
    order, how many fly in each heat, how many move on, and single or double elimination.
    Each round of the bracket is a phase of its own that holds the round's heats. The
    final is one too, even though it has one heat. The rounds are listed in the order you
    fly them.
  - **Multi-Main**: mains stacked on each other (A-Main, B-Main and so on). Each main is a
    bracket. **Seeded from** is the phase whose ranking fills the mains, such as
    Qualifying. Say how many pilots start in each main and how many **bump up** from a
    main into the one above. The dialog tells you how many mains that makes. The bottom
    main is flown first, so it is listed first. **A-Main final** lets the last heat of the
    top main use its own format, such as Chase the Ace.

  - **Round Robin**: rounds of heats where nobody goes out. Every pilot flies one heat in
    each round. Grid mixes the pilots between rounds so they meet different pilots. A pilot
    keeps the same seat in every round, so they stay on one video channel. **Points by
    place** is what each finishing place scores, first place first. Points from every heat
    are added up and the most points wins. Every heat can be flown from the start, in any
    order.

A phase's own page has **+ Add phase** too. It adds the new phase inside that phase.

**Timed by** picks the timer. **Bench** is not a timer. On a Bench heat you log passes by
hand on the Run page, which is handy for trying things with no hardware.

## Formats here

A heat or a phase is given one of the saved [formats](#/help/formats). Grid copies the
format into the event. Changing the saved format later does not change a phase that
already uses it.

A heat under a phase can use the phase's format. The line of every phase shows its format,
and on the phase's page each heat shows the format in force.

## Up next

**Up next** lists the heats that can be raced now, in a suggested order. The arrows move a
heat up or down. The order is only a suggestion. Any heat in the list can be run at any
time.

Beside a heat's name, in small grey words, is the phase it is in, like "Sport Finals ›
Semi-finals". That tells two heats with the same name apart. It is left out when the
heat's name already says it, as in "Practice Heat 3".

A phase reads **Ready** when at least one heat in it can be flown now. It reads
**Waiting** when none can yet.

## A phase's page

Open a phase to see its heats. The page shows only the heats that are right in that
phase. Each row shows a name and where it stands: Waiting, Ready, Racing, Complete or
Locked. A heat also shows who is on each seat.

If the phase holds other phases, they are in their own list at the top, **Phases in
(the phase's name)**. Click one to open its page. From there, a second link at the top
takes you back up to the phase that holds it.

The buttons for the phase itself (**+ Heat**, **Sign-up**, **Edit**, **Lock**, **Remove**) and its
**Ranking** are at the top of the heats.

- **Run** opens the heat on the [Run](#/help/run) page.
- **Ranking** opens the phase's order. For a heat you can **Set position** by hand, and
  **Disqualify** or **Reinstate** a pilot.
- **Lock** makes a phase's results official. Nothing under it can be changed until
  **Unlock**. A phase cannot be locked while it has an open protest.
- **Reopen heat** undoes calling a heat done.
- **Edit** changes a phase or a heat:
  - its **name**, at any time;
  - its **format** and the phase it sits **under**, until a race in it has been saved.
    After that Grid offers the name only, because changing the rest would change results
    already flown. Practice is the exception: it ranks nobody, so it can always be changed.
  - A bracket, a set of mains or a round robin that Grid made keeps its shape. Only its
    name, and the names of the phases inside it, can change.
  - A locked phase has no Edit. Unlock it first.
- **Remove** takes out a phase that has no races and nothing under it.

A seat that reads "Winner of Heat 3" is waiting for that heat. It fills in by itself when
the heat is done.

## Pilot sign-up

For practice and qualifying you can let pilots put themselves into heats, from their
phones. Pilots need to be [signed in](#/help/event) and phones need to
[reach Grid](#/help/network).

### Turning it on

Open the phase and press **Sign-up**, above its heats. Pick:

- **Timed by** and the **seats** every heat has. A seat is the same channel in every heat.
- **Heats open at once.** Grid keeps this many heats with room in them. Start with 3.
- **Runs for each pilot.** Leave it empty for no limit.
- **Spots held at once.** How many heats a pilot can be waiting in. Leave it empty for no
  limit.
- **Heats to sit out between two.** 0 lets a pilot fly back to back.

Press **Turn sign-up on**. The first heats appear, empty. Sign-up starts **closed**: press
**Open sign-up** on the sheet when you want pilots to start.

### The sheet

Each row is a heat and each column is a seat.

- Heats are numbered as they appear, and fly in that order.
- A new heat appears when one fills up. It also appears when you start a heat or close one,
  because those no longer have room for pilots.
- A heat that is not full can still be flown. If a heat is thin, **pull** a pilot forward
  into it: the **↑** button beside a pilot moves them to an earlier heat on the same seat,
  so their channel does not change. Ask the pilot first. Grid only tells them.
- **+ Add** puts a pilot on a free seat. Your limits are for pilots. They do not stop you.
- **Drop** takes a pilot out. Grid asks if it counts as a run. Pick **Does not count** for
  a pilot who never got to fly.
- **The heat on the timer is closed to pilots.** When you put a heat on the timer (on the
  Run page, or with **Run**), pilots can no longer join or leave it from a phone. The
  sheet marks it **On the timer**. You can still add or drop, up to the start. If you
  put another heat on the timer without flying it, it opens to pilots again.
- **Close to pilots** does the same for a heat that is not on the timer.
- Once a heat is on or flown, nobody can change who is in it.

### Runs

A run is used when a race is **saved** with the pilot in it. A race you throw out uses
none.

- **One more run for everyone** raises the limit by one.
- **+** and **−** beside a pilot give or take a run for that pilot only. Use **+** to give
  a run back to a pilot who was in a saved race but did not get to fly.

### What pilots see

On their phone, a **Sign up** tab. They press **Take** on a free seat, **Leave** on their
own, or **Next free** for the first open spot on a seat. A seat they cannot take says why:
no runs left, or sign-up is closed.

Pilots only see heats that are still open. A heat you have closed, started or flown drops
off their list. A full heat stays on it, in case someone leaves. You always see every heat.
Spectators do not see sign-up at all.

### Turning it off

**Rules**, then **Turn sign-up off**. The heats and who is in them stay.
