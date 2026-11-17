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

## Pilot sign-up

For practice and qualifying you can let pilots put themselves into heats, from their
phones. Pilots need to be [signed in](#/help/event) and phones need to
[reach Grid](#/help/network).

### Turning it on

Press **Sign-up** on a phase that holds heats. Pick:

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
- **Close to pilots** stops pilots joining or leaving one heat from a phone. Use it when
  you call the heat to the line. You can still add or drop. Starting the race closes the
  heat anyway.
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
