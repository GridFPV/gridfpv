# The Director's API, as built

**Status:** as built on the `primitives` branch, 2026-11-10. This page lists what the
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
| `GET /timers/{id}/config`, `PUT /timers/{id}/config` | A timer's settings, read from and written to the timer |
| `GET /timers/{id}/live` | Live signal and levels for tuning (server-sent events) |

**Timer settings live on the timer.** The Director keeps no copy. A write is followed by a
read, and if the timer does not then report what was sent, the answer is `Refused`.

**Live signal is not logged.** `GET /timers/{id}/live` sends `signal` events (a batch of
samples for one seat) and `levels` events (each seat's level, noise floor, enter and exit
levels, and whether it is in a crossing). It is for the Tune page and is gone once sent.

## Events

| Route | What |
|---|---|
| `GET /events`, `POST /events`, `DELETE /events/{id}` | The events |
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

- `rosters`: each class in the event, its pilots, the phases that count toward its
  standings, and the standings from 1 to n.
- `phases`: every phase with its format, its field, a heat's lineup, its ranking, and
  whether it is waiting, complete or locked.
- `races`: every race with its state, its clock times, and each seat's laps.
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
