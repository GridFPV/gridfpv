# Video

Grid can have OBS record every race, each in its own file. OBS does the filming. Grid tells
it when to start and stop, and names the file.

The Video page is under **Setup**. It is where OBS is set up, once. What was recorded is on
the open event's **Recordings** page.

## Setting it up

1. In OBS, open **Tools**, then **WebSocket Server Settings**. Turn the server on and set a
   password.
2. In Grid, on the Video page, type the **OBS address**. When OBS and Grid are on the same
   computer it is `127.0.0.1:4455`.
3. Type the **OBS password**.
4. Press **Connect**.

The pill at the top of the card turns to **Connected**. Under it Grid shows which OBS
answered and the folder OBS records into. Connect keeps the address and password and
nothing else, so you can pick scenes before you save the rest.

When the rest is set the way you want, tick **Record every race** and press **Save**.
From then on each race is recorded.

Grid never shows the password again. The field says a password is stored. Type in it only
to change it.

## What gets recorded

You choose under **What records each race**.

### OBS's own recording

OBS records what its own **Start Recording** button would record. Put Grid's overlay in
that scene and the overlay is in the video. The [Overlays](#/help/overlays) page has the
URL.

This is the simple way. Its limit: the overlay is part of the picture. If you fix a lap
later, the video still shows the old lap.

### A scene of its own, clean

Grid records one scene by itself: the **clean scene**, with the camera and no overlay.
OBS's own recording and stream are left alone, so you can stream or record something else
at the same time.

Because the video is clean, Grid can draw the overlay onto it afterwards, with the laps as
they are then. Fix a lap in Marshal, draw again, and the video is right.

To set it up:

1. Install the **Source Record** plugin in OBS, and start OBS again.
2. In OBS, make a scene with only the camera in it.
3. In Grid, pick **A scene of its own, clean**, then pick that scene as the **Clean
   scene**.
4. If you like, pick a **Scene with the overlay** too. Grid records it alongside, so you
   also have a finished video straight away.

The files are named after the race and end in `- Clean` and `- Overlay`.

## Drawing the overlay onto a recording

Open an event, then open its **Recordings** page. The **Recordings** card lists the
event's recorded races. Each one says where its overlay stands:

- **No overlay yet**: there is a clean recording, and the overlay has not been drawn.
- **Overlay drawn**: Grid drew it, with the laps as they are now.
- **Overlay filmed**: OBS filmed it live, and the laps have not changed since.
- **Laps changed**: a lap was changed after the video was made. Draw it again.
- **Overlay is in the recording**: the race was recorded with OBS's own recording, so
  there is no clean video to draw on.

Press **Draw overlay** on a race, or **Draw all** to do every race that needs it. Grid
draws one at a time and shows how far along it is. The finished video is put next to the
clean one, and its name ends in `- Overlay (redrawn)`. The clean recording is never
changed.

Under **Draw the overlay onto clean recordings** you can pick **After every race, by
itself**. Grid then draws each race when it is saved, and again when its laps change.

Drawing takes a while: about as long as the race, or longer on a slow computer.

### What Grid needs to draw

Two programs, on the computer Grid runs on:

- **A browser**: Chrome, Edge or Chromium. Windows always has Edge.
- **ffmpeg**. Grid can get it for you.

Grid looks for both by itself and shows what it found in the **Browser** and **ffmpeg**
boxes. If one is somewhere else, type where it is. If one is missing, the page says so.

When ffmpeg is missing, press **Get ffmpeg**. Grid downloads it (about 100 MB, so it needs
the internet once) and keeps it in the `tools` folder inside its data folder. This works
on Windows and on Linux. On a Mac, install ffmpeg yourself.

You can also put `ffmpeg` next to Grid, or install it so it is on the PATH.

The clean recording must be on the same computer as Grid.

## Where the videos go

Grid saves them in a folder called `videos` inside its own data folder. The Video page
shows the full path. That way your race data and your videos sit in one place.

Grid tells OBS to use that folder just for the race, then puts OBS's own folder back.

If OBS runs on a different computer than Grid, it cannot see Grid's folder. Then set
**Where videos are saved** to **OBS's own recording folder**.

## When a recording has no video in it

Sometimes OBS starts a file but no video reaches it. The file stays tiny, and OBS will not
stop it. Grid checks for this every two seconds while a scene is recorded. If it happens,
the Run page says **The recording has a problem.**

When such a recording will not stop, Grid takes the scene's Source Record filter off and
puts it back with the same settings. That lets the next race record.

The cause is in OBS. Open the scene's filters, pick the Source Record filter, and look at
its **encoder**. Then look at OBS's log (**Help**, **Log Files**) for a line about the
encoder failing to start.

## What Grid does during a race

- **Start**: when the countdown begins, Grid names the file after the race and starts the
  recording. The file is named with the event, the heat and the race number, like
  `RaceGOW 6 Track 2 race 7`.
- **Stop**: Grid keeps recording for a few seconds more, then stops. You choose how many
  seconds.
- **Save**: the recording is kept, and Grid notes it on the race.
- **Discard**, or **Call off**: the recording is moved to a folder called `trash` next to
  it. Nothing is ever deleted. Untick the trash setting to leave such files where they are.

A saved race that is discarded later has its recording moved to the trash too.

## On the Run page

- A red **REC** badge shows while OBS is recording the race.
- A yellow banner warns you **before you press Start** if the race would not be recorded:
  OBS is not connected, or OBS is already recording something else.

So you never find out afterwards that a run was not filmed.

## Runs

The event's **Recordings** page also lists the races recorded since Grid was started,
newest first. Each shows
its file and how far into the file the start tone is.

## If OBS is already recording

Grid only stops a recording it started. If OBS is already recording when a race begins,
Grid leaves it alone. That race gets no file of its own, and the run says so. Stop the
recording in OBS to have each race recorded by itself.

## OBS on another computer

Grid can start, stop and name recordings on an OBS anywhere on the network. It can only
move a file to the trash when it runs on the same computer as OBS. If it does not, a
discarded race's file stays where it is and the run says why.
