//! Dockerized-RotorHazard **min-lap neutralisation across a reconnect** e2e (#407, #438).
//!
//! RotorHazard runs its own minimum-lap rule underneath GridFPV's (`MinLapSec`, default **10 s**,
//! plus a `TIMING`/`MinLapBehavior` flag that can *discard* a sub-minimum crossing outright). A
//! discarded crossing never reaches GridFPV at all, so GridFPV's per-round floor (D26) never runs
//! on it, marshaling has nothing to restore, and #397's rejected-crossing tone stays silent for
//! exactly the crossing an RD most needs to hear about. The plugin therefore zeroes both.
//!
//! **What this target exists for (#438).** The plugin computed that neutralisation once, at load,
//! and `on_hello` replayed the resulting report on every Director (re)connect. So if the RD
//! restored `MinLapSec=10` + discard from RotorHazard's own settings screen and the Director
//! restarted or reconnected, the hello ack still said `ok: true` — and the Director's
//! [`ensure_min_lap_neutral`] takes a confirmed-neutral *plugin* report as proof and skips its own
//! socket readback. GridFPV recorded "neutralised" while the timer was discarding every sub-10s
//! crossing, until the next heat's Stage happened to re-assert the format.
//!
//! **Why the assertion is where it is.** `MinLapReport` carries no freshness marker, so a replayed
//! load-time report is *byte-identical* to a fresh one — there is no payload-level defence and no
//! payload-level test. The only observable difference is RotorHazard's own state and RotorHazard's
//! own log: post-fix the hello re-reads, finds the filter back, says so
//! ([`DRIFT_LOG`]) and re-zeroes it. So this asserts on the container log and on the values a
//! subsequent read returns, not on the ack's `ok` (which is `true` either way — that is the bug).
//!
//! It also pins the **hand-back record**: `secs_was`/`behavior_was` must still be what the RD had
//! the first time the plugin touched this server, never GridFPV's own zero read back (#454).
//!
//! Local-only class (needs Docker), gated behind `--features live` + `#[ignore]`. DISTINCT RH port
//! 5046 (the app's connect e2e uses 5042, failover 5043, no-plugin 5044, restart 5045). Run via
//! `cargo xtask live`, or:
//!
//! ```sh
//! cargo test -p gridfpv-app --features live --test rh_min_lap_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::Duration;

use gridfpv_adapters::rotorhazard::RotorHazardAdapter;
use gridfpv_adapters::rotorhazard::transport::{PluginHello, RotorHazardConnection};
use gridfpv_testkit::{NodeCsv, RhContainer, node_csv};

/// DISTINCT RH host port for the min-lap e2e (connect 5042, failover 5043, no-plugin 5044,
/// restart 5045).
const RH_PORT: u16 = 5046;
/// CSV tick interval (seconds).
const TICK: &str = "0.1";

/// How long to wait for the GridFPV plugin's `gridfpv_hello_ack` — a plugin-equipped RH answers
/// near-instantly, and this only bounds the case where nothing does.
const PLUGIN_PROBE: Duration = Duration::from_secs(5);

/// The **`MinLapSec` the RD restores** between the two connects — RotorHazard's own stock default,
/// which is the value a real timer comes back to when someone presses Reset on that settings page.
const RESTORED_MIN_LAP_SECS: i64 = 10;

/// `MinLapBehavior` = *discard the short crossing*, the half that actually loses laps.
const RESTORED_MIN_LAP_BEHAVIOR: i64 = 1;

/// The line the plugin logs when it finds the filter back and re-clears it. Its presence in
/// RotorHazard's own log is the only externally visible difference between a hello that re-verified
/// and one that replayed a load-time report — see the module docs.
const DRIFT_LOG: &str = "min-lap filter had drifted back";

/// Open a connection and take its plugin handshake, or fail loudly — a stock RH (no plugin mounted)
/// answers nothing, and this target is meaningless without one.
fn connect_and_probe(
    url: &str,
    adapter: RotorHazardAdapter,
) -> (RotorHazardConnection, PluginHello) {
    let conn = match RotorHazardConnection::connect(url, adapter) {
        Ok(conn) => conn,
        Err((e, _)) => panic!("connecting to the dockerized RotorHazard failed: {e}"),
    };
    let hello = conn.wait_for_plugin(PLUGIN_PROBE).expect(
        "the GridFPV plugin must answer gridfpv_hello — is GRIDFPV_RH_PLUGIN set / is `cargo \
         xtask live` being used?",
    );
    (conn, hello)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker (restores RotorHazard's own min-lap filter between two connects and asserts the reconnect's hello re-verifies rather than replaying its load-time report)"]
async fn a_reconnect_hello_reflects_rotorhazards_current_min_lap_state() {
    let scenario: Vec<(usize, String)> = vec![(0, node_csv(&NodeCsv::default()))];
    let rh = RhContainer::start(RH_PORT, TICK, &scenario);

    // ── 1. First connect: the plugin has already neutralised the filter at load ──────────────
    let (conn, hello) = connect_and_probe(rh.url(), RotorHazardAdapter::new());
    let first = hello
        .min_lap
        .clone()
        .expect("this plugin build reports its min-lap neutralisation in the hello ack (#407)");
    assert!(
        first.ok,
        "a freshly-loaded plugin must have cleared RotorHazard's min-lap filter: {first:?}"
    );
    assert_eq!(
        first.secs_now,
        Some(0),
        "and must confirm it by re-reading, not by trusting its own write: {first:?}"
    );
    // What the RD had before GridFPV touched this server — the hand-back record.
    let displaced = (first.secs_was, first.behavior_was);

    // ── 2. The RD restores their own filter from RotorHazard's settings screen ───────────────
    conn.set_min_lap(RESTORED_MIN_LAP_SECS)
        .expect("emit set_min_lap");
    conn.set_min_lap_behavior(RESTORED_MIN_LAP_BEHAVIOR)
        .expect("emit set_min_lap_behavior");
    // RotorHazard applies these on its gevent loop; give it a moment before dropping the socket.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !rh.logs().contains(DRIFT_LOG),
        "nothing should have re-cleared the filter yet — the drift line is the reconnect's own"
    );
    let adapter = conn.disconnect();

    // ── 3. The Director reconnects. The hello must re-verify, not replay ─────────────────────
    let (conn, hello) = connect_and_probe(rh.url(), adapter);
    let second = hello
        .min_lap
        .clone()
        .expect("the reconnect's hello ack carries a min-lap report too");

    assert!(
        rh.logs().contains(DRIFT_LOG),
        "the reconnect's hello must RE-READ RotorHazard's filter and re-clear it (#438). A \
         replayed load-time report is byte-identical to a fresh one, so this log line is the only \
         evidence there is — and without it the Director records `neutralised` while the timer \
         discards every sub-10s crossing. RotorHazard's log was:\n{}",
        rh.logs()
    );
    assert!(
        second.ok,
        "…and the re-assertion must have landed: {second:?}"
    );
    assert_eq!(
        second.secs_now,
        Some(0),
        "RotorHazard is back to a zero floor after the hello: {second:?}"
    );
    assert_eq!(
        second.behavior_now,
        Some(0),
        "…and back to highlight-don't-discard, which is the half that loses laps: {second:?}"
    );
    assert_eq!(
        (second.secs_was, second.behavior_was),
        displaced,
        "the hand-back record must still be what the RD had the FIRST time the plugin touched \
         this server (#454) — re-reading it after GridFPV's own write would report GridFPV's zero \
         back as 'what the race director had', erasing the only record of what to restore"
    );

    conn.disconnect();
    rh.stop();
}
