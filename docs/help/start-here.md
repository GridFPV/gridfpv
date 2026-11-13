# Start here

Grid runs a race day from one screen in a browser. Every page has a **?** next to its
title that opens its help. This help is built into Grid, so it works at a field with no
internet.

## Two sides: Setup and Event

A switch at the top left has two halves. The row of pages beside it changes with it.

- **Setup** holds what Grid keeps for every event. You set these up once.
- **Event** holds the event that is open. Its name is at the top right. These are the pages
  you use all race day.

Each side remembers the page you were on. Go to Setup to fix a channel, switch back to
Event, and you are on the same page as before.

## Setup, once

1. **Events**: add an event, or open one. See [Details](#/help/event).
2. **Timers**: add your timer. One that Grid finds on the network is added with one click.
3. **Seats**: pick each seat's channel and tune it so one pass makes one crossing. See
   [Seats](#/help/seats).
4. **Pilots**: add who is flying. See [Pilots](#/help/pilots).
5. **Classes**: the classes an event can race. You can skip this when flying alone.
6. **Formats**: how one race is run and how a heat is ranked. Grid comes with four. See
   [Formats](#/help/formats).

## Event, on race day

1. **Details**: set the lap rules, and who is in each class. See [Details](#/help/event).
2. **Phases**: add what will be raced: practice, qualifying, a bracket, or just one heat.
   See [Phases](#/help/phases).
3. **Run**: put a heat on the timer, start, stop, and save. See [Run](#/help/run).
4. **Marshal**: fix a lap after the race. See [Marshal](#/help/marshal).
5. **Results**: the standings. See [Results](#/help/results).

**Overlays** gives you pages to put on top of video in OBS. See [Overlays](#/help/overlays).

## Flying alone

To time yourself on a home track you need very little:

1. Under **Setup**, on **Events**, add an event.
2. On **Seats**, check that your timer is connected and Seat 1 is on your channel.
3. On **Pilots**, add yourself.
4. Under **Event**, on **Phases**, press **Add heat**. Name it after the track, pick yourself, and pick the
   format **Best 3 consecutive**.
5. On **Run**, press **Start**, fly, press **Stop**, then **Save**. Start again as often as
   you like. The heat keeps your best run.

## What is kept where

- **Pilots, classes, formats, timers and seat profiles** belong to Grid. They are there for
  every event.
- **Phases, heats, races, laps and results** belong to one event.
- **Channels and levels** live on the timer. Grid reads them from the timer and writes them
  back. A seat profile is Grid's saved copy.

## If a word is new

[Words Grid uses](#/help/words) explains seat, pass, lap, race, heat, phase and format.
