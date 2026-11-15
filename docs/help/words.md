# Words Grid uses

Grid uses a small set of words, and each one always means the same thing.

## At the gate

- **Timer**: the box at the gate. It listens for each quad and tells Grid when one goes
  through.
- **Seat**: one pilot's place on a timer. A seat has a channel and two levels, enter and
  exit. A timer with four seats can time four pilots at once.
- **Pass** (or **crossing**): one trip through the gate, on one seat.
- **Lap**: the time between two passes on the same seat.
- **Enter and exit levels**: a pass starts when the signal rises to the enter level. It ends
  when the signal falls below the exit level.
- **Profile**: a saved set of a timer's seat settings, kept under a name.

## In the competition

- **Race**: one flight, from Start to Stop. You save a race or you discard it.
- **Heat**: a lineup of pilots on seats. A heat can be raced once or many times.
- **Phase**: a part of the event that takes pilots in and gives a ranking out. Practice,
  Qualifying and a bracket are all phases. A phase can hold heats, and it can hold other
  phases.
- **Format**: the rules for the heats under it. It says how a race ends, when a heat is
  done, and how the heat is ranked.
- **Class**: a name and the pilots racing in it at this event, such as Open or Whoop.
- **Standings**: a class's order from first to last.

## Decisions people make

- **Ruling**: a decision a person makes, written down by Grid. Removing a lap, calling a
  false start and setting a position are rulings. Grid never makes one by itself.
- **Lock**: locking a phase makes its results official. Nothing under it can be changed
  until it is unlocked.
- **Protest**: a complaint against one race. A phase cannot be locked while one is open.

## Two times that are easy to mix up

- **Minimum lap time** is a rule of the event. A pass that comes too soon after the one
  before it is not counted as a lap. It is still kept, so Marshal can count it. Set it on
  the [Details](#/help/event) page.
- **Lap cooldown** is a setting of the timer. After a pass, the timer stays quiet on that
  seat for a moment, so one bouncy pass is not reported twice. Set it on the
  [Seats](#/help/seats) page. Keep it shorter than the minimum lap time.
