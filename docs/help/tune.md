# Tune a seat

Tuning sets the enter and exit levels so that every pass through the gate makes one clean
crossing. This page lets you do it on a recording, so you are not guessing.

Open it from **Tune** on a seat on the [Seats](#/help/seats) page, or from **tune** on a
seat card on the [Run](#/help/run) page.

## Record

1. Put the quad on the seat's channel and power it up.
2. Press **Start capture**.
3. Fly a few passes through the gate, the way you would in a race. Fly near the gate
   without going through it too, if your track does that.
4. Press **Stop capture**.

A recording holds up to five minutes. It is kept in this page only. It is gone when you
leave the page.

## Set the levels

The recording is drawn with two lines on it.

- The solid line is the **enter** level. A pass starts when the signal rises to it.
- The dashed line is the **exit** level. A pass ends when the signal falls below it.

Drag a line up or down, or use the **−** and **+** buttons. The arrow keys move a line that
has focus. A line moved past the other one pushes it along, so exit is always below
enter. Each **green band** is a pass the levels on screen would make. An **orange band**
was still open when the recording ended. The list under the plot gives each pass with its
peak and its width.

Aim for this:

- One band for each real pass. Not two, and not none.
- No band where you flew near the gate but not through it.
- The enter line well below the peaks and well above the noise between passes.

The bands follow the same rule the timer uses, with the timer's exit hold. The lap cooldown
is left out here on purpose. That way a bounce shows up as two bands and you can tune it
away.

**Show** picks how many seconds fill the plot. A slider scrolls a long recording.

## Apply

Nothing reaches the timer until you press **Apply to timer**. Grid reads the timer's
answer back.

- **Fixed levels** writes the two levels just as you set them.
- **A distance above the noise floor** writes the same levels as a gap above the noise
  floor. The levels then move if the noise changes during the day.

On a timer that takes levels for each seat, only this seat is changed. A timer with one
setting for all its seats gets the levels for every seat, and the page says so.
