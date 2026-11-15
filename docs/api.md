# The Director's API, as built

**Status:** as built on the `primitives` branch, 2026-11-15. This page lists what the
Director serves today. The design behind it is [Grid primitives](primitives.md). The older
[Protocol](protocol.html) doc still holds for auth, TLS and versioning, but its snapshot and
stream sections describe a design that was not rebuilt (see "What changed" at the end).

Every route answers JSON. Every failure is one shape, a `ProtocolError`:

```json
{ "code": "Refused", "message": "cannot save race 1 of Final while it is Running" }
```

| Code | HTTP | Means |
|---|---|---|
| `Unauthorized` | 401 | A write without the director's token, once a token is set |
| `UnknownScope` | 404 | No such event, pilot, class, timer, race or route |
| `BadRequest` | 400 | The request is malformed or can never work |
| `Refused` | 409 | Understood, but not allowed right now. Nothing changed |
| `Internal` | 500 | Something broke in the Director |

The `message` is written for a race director and names things by their names, never by id.
A console shows it as it is.

All wire types are generated from Rust into `bindings/`. A client imports them and never
writes one by hand.

## Reads are open, writes need the token

Any client on the network can read. Every write needs the director's token as
`Authorization: Bearer …`, once a token is set. With no token set (the desktop app on one
laptop), writes are open too.

## The Director

| Route | What |
|---|---|
| `GET /health` | `ok` |
| `GET /about` | Which build is running |
| `GET /time` | The Director's clock, in microseconds |
| `GET /diagnostics` | Where the log file is |

## Lists that outlive events

| Route | What |
|---|---|
| `GET /pilots`, `POST /pilots`, `PUT /pilots/{id}`, `DELETE /pilots/{id}` | The pilot list |
| `GET /classes`, `POST /classes`, `PUT /classes/{id}`, `DELETE /classes/{id}`, `PUT /classes/{id}/hidden` | The class list |
| `GET /timers`, `POST /timers`, `DELETE /timers/{id}` | The timer list, each with its live status |
| `GET /timers/discovered` | OFT timers found on the network, each marked if it is already added (by its own id, or by its address) |
| `GET /timers/{id}/config`, `PUT /timers/{id}/config` | A timer's settings, read from and written to the timer: seats, frequencies, thresholds, the exit hold and the lap cooldown. A write that leaves the lap cooldown out keeps the timer's. A timer that can keep its settings across a restart (`keeps_settings` in its status) is asked to keep them after every write |
| `GET /timers/{id}/live` | Live signal and levels for tuning (server-sent events) |
| `GET /timers/{id}/profiles`, `POST /timers/{id}/profiles` | A timer's saved seat profiles and which one is active; save settings as a new one |
| `PUT /timers/{id}/profiles/{pid}`, `DELETE /timers/{id}/profiles/{pid}` | Rename a profile or replace its settings; remove one. Neither writes to the timer |
| `POST /timers/{id}/profiles/{pid}/apply` | Write a profile to its timer, with the same readback as a settings write. The profile becomes the active one |
| `GET /formats`, `POST /formats`, `PUT /formats/{id}`, `DELETE /formats/{id}` | The saved formats. A new Director starts with four |

**A timer is followed to a new address.** An OFT timer has an id of its own. The Director
keeps it with the timer's record (`own_id`): from `POST /timers` when the timer was added
from the network, or else from the timer when it first connects. When a timer with that id
is found on the network at another address, the record's address is changed and the
connection is made again. The record's `id`, which heats and passes carry, never changes.
Other kinds of timer have no id of their own and stay at the address they were given.

**Timer settings live on the timer.** The Director keeps no copy. A write is followed by a
read, and if the timer does not then report what was sent, the answer is `Refused`.

**The lap cooldown is not the minimum lap.** The cooldown (`lap_cooldown_ms`, OFT's
`min_lap_ms`) is a timer setting: after a crossing on a seat the timer reports no other on
that seat for that long, which swallows the bounce of one pass. The minimum lap is the
event's lap rule (below), and the Director applies it to what the timer reports. Nothing is
written to a timer when a race starts.

**A seat profile is the Director's saved copy** of one timer's settings, kept in
`profiles.json`. Applying one is the only profile call that writes to the timer. A console
compares the active profile with the timer's settings to say whether the timer has been
changed since.

**A saved format is a template.** Setting one on a phase copies it into the event's log, so
changing or deleting a saved format never changes a phase that uses it. They are kept in
`formats.json`.

**Live signal is not logged.** `GET /timers/{id}/live` sends `signal` events (a batch of
samples for one seat) and `levels` events (each seat's level, noise floor, enter and exit
levels, and whether it is in a crossing). It is for the Tune page and is gone once sent.

## Video

| Route | What |
|---|---|
| `GET /video/settings`, `PUT /video/settings` | Whether races are recorded, where OBS is, what records (OBS's own recording, or scenes of their own), the tail after a stop, whether a discarded race's recording goes to the trash, which folder the recordings go into (`save_in`: `Grid` for the `videos` folder of the data folder, given back as `videos_folder`; `Obs` for wherever OBS records), and when the overlay is drawn onto a clean recording |
| `GET /video/status` | Whether OBS is connected (and why not), its scenes, whether it has Source Record, whether it is recording, the runs and the renders since the Director started, and whether a browser and ffmpeg were found |
| `GET /overlay/look`, `PUT /overlay/look` | How the race overlay looks, one look for the whole Director. `GET` (open) gives `placement` (`corner`: `TopLeft`, `TopRight`, `BottomLeft` or `BottomRight`; `x` and `y`, pixels in from that corner's edges, 0 to 4000; `scale`, 25 to 300 percent; `opacity`, the background, 0 to 100 percent) and whether there is a `logo_left` and a `logo_right`. `PUT` takes a placement. The live page, the replay page and every redrawn video use it |
| `GET /overlay/logo/{side}`, `PUT`, `DELETE` | The picture at the `left` or `right` end of the overlay's top line. `PUT` takes the file itself as the body (PNG, JPEG, WebP or SVG, up to 4 MB) and keeps it in the `branding` folder of the data folder. `GET` is open; `404` when there is none |
| `GET /events/{event}/races/{race}/video/{which}` | Play one of a race's videos. `which` is `recording` (the recording itself) or `overlay` (a video with the overlay in it, even when its laps are out of date). An open read that answers byte ranges, so a player can seek. Only a file the event's log names is served. `404` when the race has no such video or the file is gone. In the snapshot, a race's `video` gives `first_frame_at` for the recording and `overlaid_first_frame_at` for the overlay video: the race's `started_at` minus that is where the start sits in the file |
| `POST /video/render` | Draw the overlay onto one race's clean recording (`{event, race}`), or onto every race of the event that needs it (`{event}`). Answers how many were asked for; `409` for a race with no clean recording. The drawing runs in the background and `GET /video/status` follows it |
| `POST /video/ffmpeg` | Download ffmpeg and keep it in the `tools` folder of the data folder. Answers `202` at once; `GET /video/status` says `tools.fetching`, then `tools.ffmpeg` or `tools.fetch_problem`. `409` on a computer Grid has no build for (`tools.can_fetch` is false) |

**OBS does the capturing; the Director says when.** The Director keeps a connection to
OBS's WebSocket server (obs-websocket 5) whether or not video is on, so the scenes can be
read before recording is switched on. While video is on (`enabled`), it starts OBS's recording when a
race begins its countdown, stops it a few seconds after the race is stopped, and sets OBS's
file naming to the race's name just before the start (and puts OBS's own back after the
stop). A saved race's recording is logged into its event (`VideoRecorded`: the file, and
the Director's clock at its first frame) and shows on the race in the snapshot as `video`.
A discarded race's recording is moved to `trash/` beside it.

**A scene of its own.** With `record_from: "Scenes"` the Director leaves OBS's own
recording alone and records `clean_scene` (the camera, no overlay) through the Source
Record plugin, to `‹race› - Clean`. If `overlay_scene` is set, that scene is recorded
alongside to `‹race› - Overlay`. `VideoRecorded` then says the file is `clean`, names the
`overlay_file`, and carries `laps_key`: a mark of the race's laps as the overlay scene
filmed them.

**Drawing the overlay afterwards.** A clean recording can have the overlay drawn onto it
at any time, with the laps as they stand. The Director runs a browser with no window on
the console's replay page (`#/overlay/replay/‹event›/‹race›`), steps it through the
recording one frame at a time, and has ffmpeg lay the pictures over the clean recording.
The result is `‹race› - Overlay (redrawn).mp4` beside it, logged as `VideoRendered` with
the `laps_key` it shows. A race's `video.overlay` in the snapshot says where it stands:
`Filmed` or `Rendered` (there is an overlaid video that matches the laps, named in
`overlaid_file`), `Missing` (clean, none drawn yet), `NeedsRender` (the laps changed
since), or `CannotRender` (the recording is not clean). With `render_when: "Always"` the
Director draws every saved race of the active event that is `Missing` or `NeedsRender` by
itself. The browser and ffmpeg are looked for on the Director's machine; `browser_path`
and `ffmpeg_path` name them when they are somewhere else.

**The password is write-only.** Reads are open, so the settings a client reads say only
`has_password`. A `PUT` that leaves `obs_password` out keeps the stored one.

**The Director only stops what it started.** If OBS is already recording when a race
begins, that race gets no file of its own and its run says so.

**Files are moved only when the Director can reach them.** That is the native app on the
OBS machine. A Director elsewhere on the network still starts, stops and names recordings;
a discarded race's file is then left in place, with a note on the run.

## A copy of everything

One zip of the pilots, classes, formats, seat profiles, timers, the open event and every
event's log. The video setup, the log files and the recordings are not in it.

| Request | What it does |
| --- | --- |
| `GET /data` | `{folder?, waiting?}`: the data folder on the Director's computer, and the copy that waits to be brought in, if one does |
| `POST /data/export` | Make a copy. Answers `{name, file, copy}`: the file's name, where it is on the Director's computer (`exports` in the data folder), and what is in it. The five newest are kept |
| `GET /data/export/{name}` | Download a copy that was made |
| `POST /data/import` | Bring in a copy. The body is the zip itself. The Director checks it and keeps it; it **replaces** the data when the Director next starts, and what was there is moved to `backups` in the data folder. `400` for a file that is not a copy, `409` while a race is on |
| `DELETE /data/import` | Forget the copy that waits |

## Events

| Route | What |
|---|---|
| `GET /events`, `POST /events`, `PUT /events/{id}`, `DELETE /events/{id}` | The events. `PUT` changes the name and details |
| `GET /active-event`, `PUT /active-event` | The event every screen opens into |
| `POST /events/{id}/auth/join-token` | A read-only token for a pilot or spectator |

## The competition in an event

| Route | What |
|---|---|
| `GET /events/{id}/state` | One snapshot of everything |
| `GET /events/{id}/changes` | Tells a client when to read the snapshot again (server-sent events) |
| `POST /events/{id}/commands` | One command |
| `POST /events/{id}/presets/bracket` | Generate a bracket's phases |
| `POST /events/{id}/presets/mains` | Generate stacked mains |
| `POST /events/{id}/passes` | A gate crossing from outside (the bench, tools, tests) |
| `GET /events/{id}/signal?race=…` | A race's logged signal, for the marshal |

### The snapshot

`GET /events/{id}/state` gives an `EventView`:

- `lap_rules`: the event's minimum lap, and how long after the start crossings are
  ignored. Set with the fact `LapRulesSet`. They hold for every race of the event, flown or
  still to fly. A format may carry a minimum lap of its own, which then wins for the heats
  under it.
- `rosters`: each class in the event, its pilots, the phases that count toward its
  standings, and the standings from 1 to n.
- `phases`: every phase with its format, its field, a heat's lineup, its ranking, and
  whether it is waiting, complete or locked.
- `races`: every race with its state, its clock times, each seat's laps, and its video
  recording when the Director had one made.
- `current_race`: the race that is counting down, running, or waiting to be saved.
- `queue`: the heats that can be raced now, in the suggested order.
- `protests` and `proposed_disqualifications`: what is waiting on the director.
- `cursor`: how long the event's log was when this was worked out.
- `now`: the Director's clock at that moment.

The snapshot is worked out from the log every time it is asked for. There is no second copy
to fall out of step.

### Live updates

`GET /events/{id}/changes` is a stream of `cursor` events. Each carries one number: the
length of the log. It sends one on connect, and one more each time something a screen shows
has changed. A client that hears a new number reads the snapshot again.

That is the whole live mechanism. It carries no data, so it cannot drift from the snapshot.
A dropped connection needs no catch-up: on reconnect the first event is the current number.

**Clocks.** Countdowns and race clocks are drawn from `tone_at` and `started_at` in the
snapshot, which are on the Director's clock. A console works out how far its own clock is
from the Director's (from `now`) and counts from there. No screen runs its own race clock.

**Things that happen by themselves.** A countdown reaching its tone, a timed race running
out, a first-to-N race stopping after everyone finishes, a race reaching its time limit: the
Director logs each at the exact moment it fell due.

### Commands

`POST /events/{id}/commands` takes one `Command`:

| Command | Does |
|---|---|
| `StartRace { heat }` | Starts the next race of a heat, with a countdown if the format has one |
| `StopRace { race }` | Stops the running race |
| `SaveRace { race, needs_marshal, note }` | Saves a stopped race, so it counts |
| `DiscardRace { race, reason, note }` | Discards a race, in any state until its phase is locked |
| `FileProtest { race, by, note }` | Files a protest against a race |
| `Record(fact)` | Records a setup fact or a ruling, sent as the fact itself |

`Record` takes these facts: a phase defined or removed, a class roster, which phases count
toward a class, the run queue, a lap removed, restored, inserted or moved, a restart marked,
a heat called done or reopened, a false start, a disqualification, a position set by hand,
a phase locked or unlocked, a protest resolved, the marshal's flag on a race.

A command is checked against the event's state. If it is allowed, its facts go in the log
and the answer is the new `cursor`. If not, the answer is `Refused` with the reason, and
nothing is logged. Facts that only a timer or the Director may log (a crossing, a race start
or stop) are refused if a client sends them.

### Signal for the marshal

`GET /events/{id}/signal?race=…` gives each seat's logged signal for one race, with the race
start and the levels the timer was detecting against, all on the timer's own clock. That is
the same clock the laps are on, so the trace and the laps line up. A timer with no signal
gives seats with no samples.

## What changed from the older Protocol doc

- **No delta stream.** The older design sent a snapshot and then a stream of changes with
  resume cursors over a WebSocket. What is built sends "something changed" and the client
  reads the snapshot again. It is far simpler and cannot drift. At club scale the snapshot
  is small. Deltas can be added later behind the same feed if a big event needs them.
- **No projection list and no per-screen scopes.** There is one snapshot per event.
- **One door for writes.** The old per-thing routes for rounds, heats and channels are gone.
  Every competition write is a command.
- **Not rebuilt yet:** the read-only tier for pilots and spectators beyond the join token,
  and anything for the Cloud.
