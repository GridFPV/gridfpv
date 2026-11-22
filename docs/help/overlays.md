# Overlays

An overlay is a page you put on top of your video in OBS.

The Overlays page is under **Setup**. You set the overlays up once, and they follow
whichever event is open.

## Adding one to OBS

1. In OBS, add a **Browser** source.
2. Set its size to 1920 by 1080.
3. Copy the overlay's URL from this page and paste it in.

The overlay follows the event that is open in Grid. It only reads. Nothing you do in OBS
can change a race.

## The race overlay

It is a small panel in one corner of the video. It shows:

- the name of the run, like `RaceGOW 6 Track 2 race 7`. That is the event, the heat, and
  which race of the heat it is. The recording gets the same name.
- the race clock, and a word for what is going on: READY, STAGING, LIVE, TIME UP or
  FINISHED.
- with several pilots, a line for each one: place, laps, last lap and best lap.
- with one pilot, their laps so far and their last six laps. `HS` is the hole shot. `RS` is
  a restart you marked on the Marshal page: it shows the time up to the pass the pilot
  started again at, and it is not a lap. When the heat is ranked on laps in a row, the best
  run is shown and its laps are lit up.

A race with a time limit counts down. A small second clock shows the time since the start.

Between races it keeps the last result up for a short while. Then it waits, with the clock
at zero.

## Where it sits

Under **Placement** you choose:

- the **corner** the panel hangs from
- how far **in from the side** and **in from the top or bottom** it is, in pixels
- its **size**, from 25 to 300 percent
- how solid its **background** is. At 0 the text stands alone on the video. At 100 the
  panel is a solid sheet.

Each change is saved as you make it. The overlay in OBS follows within a few seconds. The
same placement is used when Grid redraws the overlay onto a recording, so the two match.

In a bottom corner the clock stays where you put it, and the lap list hangs below it. Move
the panel up far enough to leave room for the laps.

## Logos

You can put a picture at each end of the top line. Press **Choose a picture** and pick a
PNG, JPEG, WebP or SVG file. With no left logo, the GridFPV name shows there.

The right logo sits on a grey disc, so a dark mark still shows on the dark panel.

Logos are kept in the `branding` folder inside Grid's data folder.

## Contrast

Pick **Light text** for dark video and **Dark text** for bright video. The choice is part
of the URL, so copy the URL again after you change it.

## Preview

**Show preview** shows the overlay in a small frame on a plain backdrop. The race in it is
made up: one pilot a few laps in, with the clock running. That way you can see where the
laps go while you set the corner and the size. The overlay in OBS shows your real race.

## Copy does not work

Some browsers only allow copying on a secure page. If **Copy URL** fails, select the URL and
copy it by hand.
