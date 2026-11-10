# How Grid is tested, as built

**Status:** as built on the `primitives` branch, 2026-11-10. The older
[Testing strategy](testing-strategy.html) doc describes the test rigs of the old design
(the mock RotorHazard matrix, the golden replays). This page says what exists now.

## The layers

| Layer | Where | Runs in CI | What it proves |
|---|---|---|---|
| Rules | `crates/engine` unit tests | Yes | Every rule of the design, by replaying events: races, laps, rankings, brackets, rulings, the run queue |
| Translators | `crates/adapters` unit tests | Yes | A timer's messages become the right events, using messages copied from the spec or captured from the real thing |
| The Director's API | `crates/server` tests | Yes | Every route through the real router, with no network: snapshot, commands, refusals, presets, timers, a restart replaying the log |
| A timer connection, faked | `crates/app/tests/oft_link.rs` | Yes | The OFT connection against a small fake timer inside the test: connect, resume after a dropped link, no crossing lost or logged twice |
| A timer connection, real | `crates/app/tests/oft_live.rs` | **No, local only** | Whole races against the real OFT daemon: see below |
| The console | `frontend` unit and component tests | Yes | The pure logic of each screen, and each screen against a stand-in for the Director |
| The whole thing in a browser | `frontend/e2e` | Yes | A real browser against a real Director: add pilots, run a heat, marshal it |

## What a test may and may not fake

- **The engine fakes nothing.** It is pure: events in, state out.
- **A fake timer is a real server speaking the real protocol**, small enough to live in the
  test. It is there so CI can cover the connection code. It proves Grid's side. It cannot
  prove the timer does what the fake does.
- **So each timer also has a live test**, run by hand against the real thing. A fake is
  only trusted for what the live test has shown the real timer does.

## The live OFT test

`crates/app/tests/oft_live.rs` starts the real OFT daemon (`oftd --fake`, which runs the
real timer code against a simulated receiver) and runs:

1. **Settings:** a change is written and read back. A threshold for one seat is written,
   and the timer reports that seat using it. The timer's own refusal of a bad frequency
   comes through with its reason.
2. **A race:** laps come from the timer, lap 1 is timed from the tone, every later lap is
   the simulated 1.5 seconds to within 5 ms, the race's signal is in the log with no piece
   missing, and every crossing sits on a peak of that signal.
3. **A restart between races:** the timer's clock begins again, and the next race is still
   timed right.
4. **A restart in the middle of a race:** the laps either side are kept, and nothing is
   timed across the restart.

To run it:

```
(cd ~/development/fpv/open-fpv-timer/oft && cargo build -p oftd)
GRIDFPV_OFTD_BIN=~/development/fpv/open-fpv-timer/oft/target/debug/oftd \
  cargo test -p gridfpv-app --test oft_live -- --ignored
```

It takes about 30 seconds. It is not in CI because CI has no checkout of the OFT project.

**What it has found so far:** the first clock measurement after connecting was poor; real
signal batches start a microsecond off where the last ended; a timer restart mid-race lost
every crossing after it. All three are fixed, and each fix has a test in CI as well.

## What has not been tested

- **Real hardware.** No real OFT gate and no real receiver has been used. Everything above
  runs against a simulated receiver.
- **A real race day.** No event has been run start to finish with people.
- **More than one seat on a real timer**, and more than one timer in one race.
- **The desktop app** (`src-tauri`). It is not compiled or run by any of this.

## The gates

`cargo xtask ci` runs format, lint, every Rust test, the binding generation check and the
barrel check. The frontend adds `check`, `lint`, `test` and `build`. `cargo xtask e2e` runs
the browser suite. CI runs all of it on every push to the draft pull request.

A push is not done until CI is green. Two rules that each cost a red run:

- Never chain a commit after a gate with `;`. A failed gate must stop the commit.
- Read a gate's **exit code**, not its output. Counting "0 errors" lines missed a type
  error that only the TypeScript compiler step reported.
