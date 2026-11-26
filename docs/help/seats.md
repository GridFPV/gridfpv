# Seats

A seat is one pilot's place on a timer. The Seats page sets the channel each seat listens
on and the levels that turn a signal into a pass. The **Timers** page, next to it under
Setup, is where timers are added and removed.

With more than one timer, a box at the top picks which timer's seats are shown.

## Changes are a draft

Nothing reaches the timer while you edit. The bar at the bottom says **Not applied** until
you press **Apply to timer**. Grid then sends the settings, reads them back from the timer,
and shows what the timer says. If the timer did not take a change, Grid tells you and the
timer stays as it was. **Discard changes** throws the draft away.

If the timer's settings are changed somewhere else, such as on the timer's own page, Grid
reads them again and shows them. If you had a draft, Grid keeps it and tells you the timer
changed.

## When another program has control

A timer lets one program at a time change its settings. That stops two programs from
undoing each other's work. The first program to ask gets control. Grid asks every time it
connects to a timer.

If another program asked first, such as RotorHazard, Grid tells you its name at the top of
the page. Grid still shows the timer's settings and still times laps on it. It cannot
change the timer, so **Apply to timer** and the profile box are off.

To give control to Grid:

1. Press **Open the timer's page** in the notice.
2. On the timer's page, find the list of programs that asked for control.
3. Pick the one whose name starts with **GridFPV**. The notice in Grid says the exact name.

Grid can change the timer again right away. This is not a lock against other people. It
only keeps well-behaved programs out of each other's way.

## Profiles

A profile is a saved copy of a timer's settings: the channels, the levels and the timer
settings. Save one for each place you fly.

- Picking a profile in the box **writes it to the timer**.
- **Save** stores what the timer has now over the profile. It is on when the timer has
  been changed since the profile was saved.
- **Save as new** stores what the timer has now under a new name.
- **Rename** and **Delete** change only the saved copy. The timer is left as it is.

## Timer settings

These belong to the timer, whatever is flown on it. They are at the top of the page,
above the seats, because every seat uses them unless it has levels of its own.

- **Follow the noise floor**: the enter level sits a set number of dB above the noise
  floor and moves with it. This is the easy choice and works in most places.
- **Fixed levels**: you type the enter and exit levels yourself.
- **Exit hold**: the signal must stay below the exit level this long before a pass ends. A
  longer hold stops one pass from being split in two.
- **Lap cooldown**: after a pass, the timer reports no other pass on that seat for this
  long. It swallows the bounce of one pass. Two seconds is a good start. Keep it shorter
  than the event's minimum lap time, which is on the [Details](#/help/event) page.

## Seats

Each seat has a channel, an on/off switch, its live numbers, and its own signal.

- Pick a channel from the box, or pick **Other frequency** and type the MHz.
- Turn a seat **Off** when nobody flies on it. Fewer seats on one receiver means each seat
  is checked more often, which makes lap times more exact.
- **Level** is the signal right now. **Noise floor** is the signal with no quad near.
- **Tune** opens the seat's own page, where you record its signal and set its levels on the
  recording. See [Tune a seat](#/help/tune).
- On a timer that can do it, **This seat's levels** gives one seat its own enter and exit
  levels. Other seats use the timer's.
- **The signal** is drawn under each seat that is on: the last 10 seconds. The solid line
  is the enter level and the dashed line is the exit level. A shaded band is a pass. It is
  right there with the seat's settings, so you see what a change does.

## Timers

On the **Timers** page, a timer that Grid finds on the network is listed under **Found on
the network**. Press **Add**. A timer can also be added by its address. **Settings** opens
that timer's seats. If a timer gets a new address from the network, Grid finds it again by
itself and keeps using it. You do not add it a second time. **Remove** takes a timer out of
Grid. Its laps stay in the events it
timed.
