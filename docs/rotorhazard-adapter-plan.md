# RotorHazard adapter: the plan for the rewrite

**Status:** a plan, written 2026-11-10, worked out by reading RotorHazard's own source for
4.4.0 and 4.3.0. The pure translator is being built; the connection is not.

**Checked against live RotorHazard on 2026-11-10** (the 4.4.0 and 4.3.0 images on this
machine, by hand, with a throwaway script):

- A websocket-only connection works, with no login, and `get_server_time` answers.
- `stage_race` with `secondary_format`, `ignore_secondary_heat` and `start_time_s` started
  the race at exactly the instant asked for (the reported start minus the asked start was
  0.0 on both versions).
- A lap of 0.6 seconds was kept, so the minimum lap filter is off in that mode.
- `stop_race` took the status to "done" and `discard_laps` took it back to "ready" with the
  lap list empty.
- 4.4.0 laps carry `deleted` and `source`; 4.3.0 laps carry `lap_raw` instead. Both carry
  `lap_time_stamp`.

Everything else below is still "read in the source", not "seen working".

Source paths are in RotorHazard's `src/server/`. Line numbers are for 4.4.0.

## The main finding: no plugin is needed

Stock RotorHazard already lets an outside program command it. One `stage_race` message can
start a race with a plain format, at a moment Grid picks, with RotorHazard's own minimum lap
filter switched off. That path is in both 4.3.0 and 4.4.0.

So Grid can talk to an untouched RotorHazard over its normal socket. Nothing has to be
installed on the timer.

What the old plugin did that is no longer needed, now that Grid owns heats and results:
making its own race format, sending passes, seating pilots in RotorHazard heats, the
install helper, and restarting the server.

The one thing lost without a plugin is the **dense signal trace**. Stock RotorHazard only
sends each seat's level ten times a second. That is enough for tuning and a rough race
trace. A fine trace would need a plugin later.

## How a race runs

| Grid does | RotorHazard is told | Notes |
|---|---|---|
| Start a race | `discard_laps`, then `stage_race` with `secondary_format: true`, `ignore_secondary_heat: true` and `start_time_s` set to Grid's tone | The built-in "secondary" format has no time limit, no win rule and no tones (`server.py:4025-4037`). It skips the minimum lap filter (`RHRace.py:824-827`) |
| The tone | nothing | RotorHazard plays no tones for a commanded start (`RHRace.py:302-308`). Grid's console plays the only one |
| Stop | `stop_race` | |
| Save or discard | `discard_laps` | Grid never tells RotorHazard to save, so RotorHazard's own heats and results are never written |

- **Start time.** RotorHazard starts exactly when told, if that is more than 0.9 seconds
  away (`RHRace.py:303-304`). Sooner than that and it starts 0.9 seconds after the message,
  and drops crossings before then. So a start with no countdown has a short blind spot.
- **Proof the start landed.** RotorHazard answers with `stage_ready` and then its race
  status changes to racing. If that does not happen in 2 seconds, the timer's status shows a
  problem.
- **Someone presses a button on RotorHazard's own page.** Lap times are absolute, so
  RotorHazard's race only decides whether it is listening. If it stops listening while
  Grid's race runs, the adapter starts it again and reports the gap.

## Laps become crossings

- RotorHazard sends its whole lap list on every crossing (`RHRace.py:972`). Each lap has a
  time in milliseconds since the race start, on RotorHazard's own clock.
- A crossing's time is the race start plus that. Its seat is the node's number.
- RotorHazard renumbers laps when one is deleted, so the lap number is not used. A crossing
  is known by its seat and its time.
- After a dropped link the adapter asks for the lap list again and ignores what it has
  already logged.
- Laps deleted on RotorHazard's own page are ignored. Grid does its own rulings.

## Clock

`get_server_time` gives RotorHazard's clock (`server.py:2332-2335`). The adapter asks every
two seconds and measures the offset the same way it does for OFT.

## Seats and settings

- A node is a seat. Frequency is written with `set_frequency` and read back from
  `frequency_data`. Frequency 0 means the seat is off.
- Enter and exit levels are written per node and read back with `load_data`. RotorHazard
  does not confirm a write by itself, so the adapter asks until what it reads matches.
- RotorHazard's levels are **per seat**, and are plain counts with no unit. Grid's timer
  settings today hold one threshold for the whole timer (the OFT shape). They need a
  per-seat threshold and a unit. That also fits the next OFT version.
- **One of the saved facts was wrong.** The old notes said a frequency write keeps
  RotorHazard's own band and channel label. The source says it clears the label unless the
  write names one (`server.py:993-994`).

## 4.3.0 and 4.4.0

The messages this plan uses are the same on both. The differences it touches are small:
the "no heat" value, a few extra fields on a lap, and whether a failed start sends a status.
No version switch is needed.

## Testing

- **In CI:** unit tests for the translator using the saved fixtures, and a fake RotorHazard
  inside the test, like the fake OFT timer.
- **On this machine only:** tests against the two RotorHazard images already built here,
  using RotorHazard's own "simulate a lap" button to make crossings.

## Order of work

1. The socket framing RotorHazard uses, as pure code. Before anything else: one real
   connection to each image, to prove the framing.
2. The translator and its tests.
3. Timer settings: a RotorHazard kind, a per-seat threshold, a unit. The Tune page follows.
4. The connection: connect, retry, status, clock, crossings.
5. Race control: start, stop, discard, and putting RotorHazard right when it disagrees.
6. Settings read and write, each write read back.
7. Live levels for the Tune page, and the rough race trace.
8. Tests against both images. Update the rules in `CLAUDE.md` that still name the old file.

## Decisions for Ryan

Each has the default that will be built unless he says otherwise.

1. **No plugin.** Default: none. RotorHazard is used as it ships.
2. **Starts with no countdown.** RotorHazard cannot hear the first 0.9 seconds. Default:
   when a RotorHazard timer is in the heat, the countdown is at least 1.5 seconds.
3. **RotorHazard's own channel label.** Default: let a frequency write clear it. Grid shows
   its own label.
4. **Someone stops RotorHazard in the middle of Grid's race.** Default: start it again by
   itself and report the gap.
5. **The rough ten-a-second signal.** Default: log it with the race, marked as counts, so
   Marshal has something to show. A fine trace waits for a plugin.
6. **RotorHazard 4.3.0.** Default: keep supporting it. It costs nothing in this design.

Already decided and built: a crossing that arrives just after a stop still belongs to that
race (this helps OFT as well).
