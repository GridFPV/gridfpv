# Phases

The Phases page holds everything that is raced at the event.

A **heat** is a lineup of pilots on seats. It can be raced once or many times. A **phase**
is a part of the event that takes pilots in and gives a ranking out. Practice, Qualifying
and a bracket are phases. A phase can hold heats, and it can hold other phases.

## Adding things

- **Add heat**: one heat. Give it a name, pick what times it, pick the pilots in seat
  order, and pick a format. The first pilot gets Seat 1. **Start from a class** fills in a
  class's roster. A heat can sit at the top of the event or **under** a phase.
- **Add phase**: a phase that holds heats. Pick its format once and every heat under it
  uses it. **For the class** makes the class's roster the phase's field, so every pilot in
  the class gets a place. Then add heats with **+ Heat** on the phase's row.
- **Add bracket**: every heat of an elimination bracket at once. Pick the pilots in seed
  order, how many fly in each heat, how many move on, and single or double elimination.

**Timed by** picks the timer. **Bench** is not a timer. On a Bench heat you log passes by
hand on the Run page, which is handy for trying things with no hardware.

## Formats here

A heat or a phase is given one of the saved [formats](#/help/formats). Grid copies the
format into the event. Changing the saved format later does not change a phase that
already uses it.

A heat under a phase can use the phase's format. The row of every phase shows the format
in force.

## Up next

**Up next** lists the heats that can be raced now, in a suggested order. The arrows move a
heat up or down. The order is only a suggestion. Any heat in the list can be run at any
time.

## The tree

Each row shows a phase's name and where it stands: Waiting, Ready, Racing, Complete or
Locked.

- **Run** opens the heat on the [Run](#/help/run) page.
- **Ranking** opens the phase's order. For a heat you can **Set position** by hand, and
  **Disqualify** or **Reinstate** a pilot.
- **Lock** makes a phase's results official. Nothing under it can be changed until
  **Unlock**. A phase cannot be locked while it has an open protest.
- **Reopen heat** undoes calling a heat done.
- **Remove** takes out a phase that has no races and nothing under it.

A seat that reads "Winner of Heat 3" is waiting for that heat. It fills in by itself when
the heat is done.
