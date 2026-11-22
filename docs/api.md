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
| `Unauthorized` | 401 | A write without the director's token, once a token is set. Or anything but the public reads, from another device with no token |
| `Forbidden` | 403 | A change asked for by a page on another website |
| `TooManyRequests` | 429 | Too many wrong access tokens from one device. It can try again after a minute |
| `UnknownScope` | 404 | No such event, pilot, class, timer, race or route |
| `BadRequest` | 400 | The request is malformed or can never work |
| `Refused` | 409 | Understood, but not allowed right now. Nothing changed |
| `Internal` | 500 | Something broke in the Director |

The `message` is written for a race director and names things by their names, never by id.
A console shows it as it is.

All wire types are generated from Rust into `bindings/`. A client imports them and never
writes one by hand.

## Who may do what

There are three ways in, each a listener on its own port (`crates/server/src/access.rs`,
`docs/primitives.md` §11.10). The way a request came in decides what it may do.

| Way in | Who comes in by it | What they may do |
|---|---|---|
| **The Grid computer's own** (`127.0.0.1:8080`) | The director, at the Grid computer | Everything. No token is asked for |
| **Public** (port 80, or 8081) | Spectators, pilots, kiosks | Read the console's files, `/health`, `/about`, `/time` and everything under `/public` but the director's sign-in. Nothing else, **whatever token or cookie is sent** |
| **Remote RD** (port 8082) | The director, on another device | With the access token: everything. Without it: the console's files, `GET /public/hello`, `/public/session`, `/health`, `/about`, `/time`. Nothing about the race |

The Grid computer's own listener only ever binds a loopback address. A caller there is
**local** when all three hold: the connection comes from a loopback address; the request
was not passed on by a proxy (no `Forwarded`, `X-Forwarded-*`, `X-Real-IP` or `Via`
header); and the `Host` it asked for is `localhost` or a loopback address. A caller there
who fails one of these (a proxy on the Grid computer, a page that points its own name at
`127.0.0.1`) is a visitor: public reads only. Nobody who comes in by Public or Remote RD is
ever local, even from the Grid computer.

`hello.caller` names the result: `Local`, `Director` (the token was shown) or `Visitor`.

**The access token** belongs to the Remote RD door. Grid makes it when the door is first
opened, and it is changed on the Network page. It is sent as `Authorization: Bearer
<token>`, at the Remote RD door only. For scripts: `curl -H "Authorization: Bearer <token>"
http://<address>:8082/events`.

A browser at the Remote RD door signs in once and then carries a cookie:

| Route | What it does |
|---|---|
| `GET /public/hello` | `{version, caller, door?, event?, director_sign_in, kiosk}`. `door` is `Public` or `Remote`, and absent on the Grid computer's own address. `event` is the open event's name; it is left out at the Remote RD door until the token is shown. `director_sign_in` says whether a director can sign in at this address |
| `POST /public/session` | Sign in. Send the token as a `Bearer` header. Answers `204` and sets the cookie `gridfpv_director` (`HttpOnly`, `SameSite=Strict`, half a day). The cookie holds a sign-in of its own, never the token. A wrong token answers `401`. At the Public door this route answers `401` to everyone |
| `DELETE /public/session` | Sign out. The sign-in stops working at once. Always `204` |

A browser sends a cookie to every port of an address, so a director's browser carries the
sign-in to the Public door too. It counts for nothing there.

`GRIDFPV_RD_TOKEN`, set when the Director is started, is a separate and older thing, kept
for the test harness: with it set, every change needs that token **even on the Grid
computer**. It is not how a director signs in from another device.

A visitor can read the **one event that is open**, and only that. These give the console's
own shapes with the director's things taken out, so the same page code draws both
(`crates/server/src/spectate.rs`).

| Route | What it gives |
|---|---|
| `GET /public/event` | `{event?: {id, name}}`: the open event. Empty when none is |
| `GET /public/event/state` | The snapshot (`EventView`). Left out: each race's video, note and to-do flag, crossings that do not count, protests, proposed disqualifications |
| `GET /public/event/changes` | The same change feed as `GET /events/{id}/changes`, for the open event |
| `GET /public/event/pilots` | The pilots the snapshot names: `id`, `callsign` and `color` only |
| `GET /public/event/classes` | The classes in the event: name only |
| `GET /public/qr` | An SVG picture: a QR code that opens the public pages, in Grid's colours. It holds the address the caller asked at (or the Public door's own, for a caller on the Grid computer) and nothing else. `409` when there is no address to give |
| `GET /public/event/timers` | The timers its heats are flown on: name and seats only. No address |
| `GET /public/event/channels` | `[{adapter, seat, freq_mhz}]`: the video channel of each switched-on seat on those timers. A bench seat has none. The Director asks a timer at most once in 15 seconds, however many are asking, and keeps the last answer when a timer does not reply |

With no event open, all but the first answer `404` with "No event is open." There is no way
to ask for another event by id. A field added to one of those shapes later does not compile
until someone decides, in `spectate.rs`, whether a visitor may see it.

A **pilot** signs in with a short code that is theirs for one event
(`crates/server/src/pilot_access.rs`). It says who they are and opens nothing else: a
signed-in pilot is still a visitor to every route outside `/public`.

| Route | Who | What it does |
|---|---|---|
| `GET /events/{id}/access` | Director | `{pilots: [{pilot, code, signed_in}], address?}`: every pilot in the event with their code. A code is made the first time this is read. `address` is where a phone opens Grid, when it can. Sent with `Cache-Control: no-store` |
| `POST /events/{id}/access/{pilot}/reset` | Director | A new code for one pilot. The old one stops working and their device is signed out |
| `DELETE /events/{id}/access/{pilot}/session` | Director | Sign a pilot's device out. Their code still works. `204` |
| `GET /events/{id}/access/{pilot}/qr` | Director | The code as a QR picture (SVG). It holds `address` + `#/join/CODE`. `409` while other devices cannot reach Grid |
| `POST /public/pilot/session` | Anyone | `{code}`. A right code for the open event answers `{id, callsign}` and sets the cookie `gridfpv_pilot` (`HttpOnly`, `SameSite=Strict`). A wrong one answers `401`. With no event open, `404` |
| `DELETE /public/pilot/session` | Anyone | Sign out. `204` |

`GET /public/hello` gains `pilot?: {id, callsign}` for a browser that is signed in.

A code is six characters from `ABCDEFGHJKLMNPQRSTUVWXYZ23456789`. As typed, case, spaces
and dashes do not matter. One device at a time: a second sign-in with the same code
replaces the first. A sign-in is for the event it was made in, and ends when the pilot
leaves the event. Wrong codes are counted by address, apart from wrong tokens, with the
same limit. The codes and sign-ins are kept in `pilot-access.json` in the data folder, not
in any event's log and not in a copy made on the Data page.

**Sign-up** (`docs/primitives.md` §11.5). The rules are in the engine
(`crates/engine/src/signup.rs`). A spot is a seat in a heat's lineup, so sign-up writes
plain heats; four facts carry what a lineup cannot (`SignupSet`, `SignupRunsSet`,
`SignupCharged`, `SignupHeatClosed`), and they can only come from a sign-up command.

The **director** sends `POST /events/{id}/commands` with `{"Signup": …}`:

| Command | Does |
|---|---|
| `Set { phase, rules? }` | Turn sign-up on, change its rules, or (no `rules`) turn it off. `rules` is `{seats, open_heats, held_limit?, run_limit?, rest_heats, open}` |
| `Take { heat, seat, pilot, by }` | Put a pilot on a seat. `by` is `Director` or `Pilot` |
| `TakeNext { phase, seat, pilot, by }` | The first free spot on that seat |
| `Leave { heat, pilot, by }` | Give a spot up |
| `Drop { heat, pilot, counted }` | Take a pilot out. `counted` uses up one of their runs |
| `Pull { pilot, from, to }` | Move a pilot forward to an earlier heat, on the same seat |
| `SetRuns { phase, pilot, extra }` | One pilot's own allowance, added to the limit |
| `CloseHeat { heat, closed }` | Close a heat to pilots, or open it again |

**The kiosk** (§11.4) is a tablet the director sets up. Only the director makes one.

| Route | Who | Does |
|---|---|---|
| `GET /kiosk` | Director | `{codes: [..], kiosks: [{number, set_up_at, name?}]}`: the kiosk codes not used yet, and the tablets set up |
| `POST /kiosk/codes` | Director | Make one more kiosk code (eight letters and digits). Answers the list. `409` when twelve are already waiting |
| `DELETE /kiosk/codes/{code}` | Director | Cancel a code that has not been used. Answers the list |
| `PUT /kiosk/devices/{number}` | Director | `{name}`: name a kiosk (trimmed, 40 characters at most; empty takes the name away). Answers the list. `404` for a kiosk that is not there |
| `DELETE /kiosk/devices/{number}` | Director | Remove a kiosk: its tablet stops being one. Answers the list |
| `POST /public/pilot/kiosk/device` | Anyone | `{code}`: a kiosk code. A right one is **used up**, sets the `gridfpv_kiosk` cookie (a token of that tablet's own, `HttpOnly`) and answers `204`. A wrong or used one answers `401`; wrong ones are counted with wrong pilot codes |
| `DELETE /public/pilot/kiosk/device` | Anyone | This device stops being a kiosk. `204` always |

`GET /public/hello` says `kiosk: true` to a tablet that is set up, with `kiosk_name`: the
name the director gave it, or "Kiosk 2".

On a kiosk, `POST /public/pilot/kiosk` with `{code}` checks a **pilot's** code like a
sign-in (same `401`, same count of wrong codes, same `429`) but sets no cookie and leaves
the pilot's own device signed in. It answers `{pilot, pass, idle_seconds}`. A device that
is not a kiosk gets `403`, whatever the code. The kiosk sends the pass in the
`X-GridFPV-Kiosk` header on the pilot's routes below. When that header is there it alone
decides who the pilot is: a pass that is wrong or has run out, or that comes from a device
that is not a kiosk, answers `401`, whatever other cookie the tablet holds. A pass is kept
in memory only and stops `idle_seconds` after it was last used. `DELETE
/public/pilot/kiosk` with the header ends it (`204` always).

A **pilot** uses three routes of their own. The pilot is the one the request is signed in
as, never one named in the body, and it always acts as `Pilot`, so every limit applies:

| Route | Body |
|---|---|
| `POST /public/pilot/signup/take` | `{heat, seat}` |
| `POST /public/pilot/signup/next` | `{phase, seat}` |
| `POST /public/pilot/signup/leave` | `{heat}` |

With no sign-in they answer `401`. A change the rules do not allow answers `409` with the
reason in words. The snapshot carries the sheet on the phase: `signup: {rules, heats:
[{heat, number, seats, state, full}], pilots: [{pilot, used, held, extra, allowed?}]}`.

Ten wrong tokens from one address inside a minute shut that address out (`429`) until the
minute has passed, whatever it sends. The public reads still answer it.

A request that changes something and carries an `Origin` from another site is refused
(`403`), whoever the caller is. A page on the Grid computer (`localhost`, a loopback
address, the desktop app's window) and the Director's own pages are allowed. The same rule
decides which origins get a CORS answer.

## Other devices on the network

The Director's own listener is on its own computer only. Two more listeners, the **doors**,
let other devices in. Each is off until the director switches it on.

| Route | What it does |
|---|---|
| `GET /network` | `{public, remote, token?}`. Each door is `{enabled, port, port_picked, state, addresses, problem?, note?}`. `state` is `Off`, `Starting`, `On` or `Failed`. `addresses` are the URLs to open on another device. `problem` says in words why the listener is not up. `note` says when Grid is on its second choice of port. `token` is the access token, and is in the answer **only on the Grid computer** |
| `PUT /network/public`, `PUT /network/remote` | `{enabled, port?}`. Leave `port` out to keep it; `0` lets Grid pick again. A picked port must not be the Director's own or the other door's. Opening the Remote RD door with no token makes one. Shutting it signs every other device out. Answers the same shape as the `GET` |
| `PUT /network/address` | `{address?}`. Sets the public address every public link and QR code uses: `http` or `https`, a name, and at most a port. Left out or empty clears it. `GET /network` answers it as `public_address`, and it is first in the Public door's `addresses` |
| `PUT /network/token` | `{token?}`. Changes the access token to the one given (8 to 64 characters, no spaces), or to a new one Grid makes. Signs every other device out and closes what they had open. **Only the Grid computer may** (`403` otherwise) |

When no port is picked, Public tries **80**, then **8081**; Remote RD uses **8082**.

Switching a door off closes its listener and every connection it had open. A port that is
taken gives `Failed` with the reason, and the Director tries again every few seconds. The
settings and the token are kept in `network.json` in the data folder. They are not part of
a copy made on the Data page.

The Director's own listener cannot be put on the network. `GRIDFPV_ADDR` picks its port, and
must be a loopback address; anything else and the Director does not start. To use a
Director on a machine with no screen, open an SSH tunnel to it
(`ssh -L 8080:127.0.0.1:8080 <machine>`) and browse to `http://localhost:8080`.

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
| `GET /classes`, `POST /classes`, `PUT /classes/{id}`, `DELETE /classes/{id}`, `PUT /classes/{id}/hidden` | The class list. A class is `{id, name, org?, reference?, description?, builtin?, hidden?}`. `org` is free text, 24 characters at most, shown as a tag; in a `PUT`, `null` clears it |
| `GET /classes/builtins`, `POST /classes/builtins` | `GET` answers `{removed}`: how many of the classes that come with Grid have been removed. `POST` puts them all back, as shipped, and answers the class list. A class that came with Grid (`builtin: true`) is edited and removed like any other |
| `GET /timers`, `POST /timers`, `DELETE /timers/{id}` | The timer list, each with its live status |
| `GET /timers/discovered` | OFT timers found on the network, each marked if it is already added (by its own id, or by its address) |
| `GET /timers/{id}/config`, `PUT /timers/{id}/config` | A timer's settings, read from and written to the timer: seats, frequencies, thresholds, the exit hold and the lap cooldown. The pass window is not here: it is the event's lap rule, and a write keeps the one the timer has. A write that leaves the lap cooldown out keeps the timer's. A timer that can keep its settings across a restart (`keeps_settings` in its status) is asked to keep them after every write |
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

**One program at a time changes an OFT timer.** A timer's status (`GET /timers`) carries
`control`: `{"holder":"Nobody"}`, `{"holder":"Grid"}` or `{"holder":"Other","name":"…"}`.
It is absent on a timer that lets anyone write, and while a timer is not connected.
`control_name` is the name the Director asked for control under, as the timer's own page
lists it. While another program has control, every write to that timer (`PUT
/timers/{id}/config`, applying a profile) is `Refused` with a sentence naming the program,
and nothing is sent to the timer. Control is moved on the timer's own page, never through
this API. `config_rev` is a count the timer moves whenever its settings change, whoever
changed them; read `GET /timers/{id}/config` again when it moves.

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
| `POST /events/{id}/presets/round-robin` | Generate a round robin |
| `POST /events/{id}/passes` | A gate crossing from outside (the bench, tools, tests) |
| `GET /events/{id}/signal?race=…` | A race's logged signal, for the marshal |

### The snapshot

`GET /events/{id}/state` gives an `EventView`:

- `lap_rules`: the event's minimum lap, and how long after the start crossings are
  ignored. Set with the fact `LapRulesSet`. They hold for every race of the event, flown or
  still to fly. A format may carry a minimum lap of its own, which then wins for the heats
  under it. `pass_window_micros` is the pass window (10 seconds at most). The timer applies
  it: the Director writes it to each connected timer that has one (`pass_window` in the
  timer's status) and notes what the timer then has (`pass_window_ms`) or why it could not
  be set (`pass_window_problem`). A timer another program has control of (`control` in
  the timer's status, below) cannot be written to; when its window is not the event's, the
  problem says so.
- `rosters`: each class in the event, its pilots, the phases that count toward its
  standings, and the standings from 1 to n.
- `phases`: every phase with its format, its field, a heat's lineup, its ranking, and
  whether it is waiting, complete or locked.
- `races`: every race with its state, its clock times, each seat's laps, and its video
  recording when the Director had one made.
- `current_race`: the race that is counting down, running, or waiting to be saved.
- `on_timer`: the heat the director is running now, whether its race has started or not.
  It is the heat of `current_race` when there is one; otherwise the heat last put on the
  timer or last started. Absent until a heat has been put there. Set with the command
  `{"PutOnTimer": {"heat": …}}`, which is refused (`409`) while a race of another heat is
  waiting to be saved or thrown out. A sign-up heat on the timer is closed to pilots.
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
