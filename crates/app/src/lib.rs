//! GridFPV walking-skeleton application logic.
//!
//! Wires the append-only spine end to end:
//! 1. a **synthetic source** appends `Pass` events to the log (#8) — standing in
//!    for a real timer adapter, which arrives in v0.2;
//! 2. the events are read back and the **projection engine** derives a lap list,
//!    proving the result is recomputable from the log alone;
//! 3. a **minimal read** renders the lap list as text (#9).
//!
//! The same pieces back the replay harness (`tests/replay.rs`, #10).
#![forbid(unsafe_code)]

use gridfpv_events::{AdapterId, CompetitorRef, Event, GateIndex, Pass, SourceTime};
use gridfpv_projection::{LapList, lap_list};
use gridfpv_storage::{EventLog, Result as StorageResult};

/// One synthetic pilot: a display/source name and the duration (microseconds) of
/// each lap it flies.
pub struct SyntheticPilot<'a> {
    /// Source-local competitor handle.
    pub name: &'a str,
    /// Lap durations in microseconds, in order.
    pub lap_micros: &'a [i64],
}

/// Generate a deterministic synthetic session: an `AdapterConnected`, a
/// `CompetitorSeen` per pilot, and a run of lap-gate `Pass` events whose spacing
/// matches each pilot's lap durations.
///
/// A pilot flying `n` laps emits `n + 1` lap-gate passes (the first starts the
/// clock). Timestamps are on a single synthetic source clock starting at 0;
/// per-pilot monotonic `sequence` numbers disambiguate ordering.
pub fn synthetic_session(adapter: &str, pilots: &[SyntheticPilot<'_>]) -> Vec<Event> {
    let adapter_id = AdapterId(adapter.to_string());
    let mut events = vec![Event::AdapterConnected {
        adapter: adapter_id.clone(),
    }];

    for pilot in pilots {
        let competitor = CompetitorRef(pilot.name.to_string());
        events.push(Event::CompetitorSeen {
            adapter: adapter_id.clone(),
            competitor: competitor.clone(),
        });

        // First pass starts the clock; each lap advances time by its duration.
        let mut at = 0i64;
        let mut sequence = 0u64;
        events.push(make_pass(&adapter_id, &competitor, at, sequence));
        for &duration in pilot.lap_micros {
            at += duration;
            sequence += 1;
            events.push(make_pass(&adapter_id, &competitor, at, sequence));
        }
    }

    events
}

fn make_pass(adapter: &AdapterId, competitor: &CompetitorRef, at: i64, sequence: u64) -> Event {
    Event::Pass(Pass {
        adapter: adapter.clone(),
        competitor: competitor.clone(),
        at: SourceTime::from_micros(at),
        sequence: Some(sequence),
        gate: GateIndex::LAP,
        signal: None,
    })
}

/// Append every event to the log, then read the log back and derive the lap list.
///
/// Reading from the log (rather than projecting `events` directly) is the point of
/// the walking skeleton: it proves the projection is recomputable from the durable
/// append-only log, exactly as it will be after a restart.
pub fn append_and_project<L: EventLog>(log: &mut L, events: &[Event]) -> StorageResult<LapList> {
    for event in events {
        log.append(event.clone(), None)?;
    }
    let replayed: Vec<Event> = log.read_all()?.into_iter().map(|s| s.event).collect();
    Ok(lap_list(&replayed))
}

/// Render a lap list as plain text — the minimal read (#9).
pub fn render_lap_list(laps: &LapList) -> String {
    if laps.competitors.is_empty() {
        return "(no laps)".to_string();
    }
    let mut out = String::new();
    for competitor in &laps.competitors {
        let key = &competitor.competitor;
        out.push_str(&format!("{} · {}\n", key.adapter.0, key.competitor.0));
        if competitor.laps.is_empty() {
            out.push_str("  (no completed laps)\n");
        }
        for lap in &competitor.laps {
            out.push_str(&format!(
                "  lap {:>2}  {}\n",
                lap.number,
                format_micros(lap.duration_micros)
            ));
        }
        if let Some(best) = competitor.best() {
            out.push_str(&format!(
                "  best {}  ·  total {}\n",
                format_micros(best.duration_micros),
                format_micros(competitor.total_micros())
            ));
        }
    }
    out
}

/// Format a microsecond duration as `S.mmm s` (display only — math stays in µs).
fn format_micros(micros: i64) -> String {
    let secs = micros / 1_000_000;
    let millis = (micros % 1_000_000) / 1_000;
    format!("{secs}.{millis:03} s")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_storage::InMemoryLog;

    fn sprint() -> Vec<Event> {
        synthetic_session(
            "sim",
            &[
                SyntheticPilot {
                    name: "Ace",
                    lap_micros: &[30_000_000, 31_000_000],
                },
                SyntheticPilot {
                    name: "Bee",
                    lap_micros: &[33_000_000],
                },
            ],
        )
    }

    #[test]
    fn synthetic_session_emits_lifecycle_and_passes() {
        let events = sprint();
        // 1 AdapterConnected + per pilot: 1 CompetitorSeen + (laps+1) passes.
        // Ace: 1 + 3 passes; Bee: 1 + 2 passes => 1 + 4 + 3 = 8.
        assert_eq!(events.len(), 8);
        assert!(matches!(events[0], Event::AdapterConnected { .. }));
    }

    #[test]
    fn spine_round_trips_through_the_log() {
        let mut log = InMemoryLog::new();
        let laps = append_and_project(&mut log, &sprint()).unwrap();
        // Ace flew 2 laps, Bee 1.
        let ace = &laps.competitors[0];
        assert_eq!(ace.competitor.competitor.0, "Ace");
        assert_eq!(
            ace.laps
                .iter()
                .map(|l| l.duration_micros)
                .collect::<Vec<_>>(),
            vec![30_000_000, 31_000_000]
        );
        let bee = &laps.competitors[1];
        assert_eq!(bee.competitor.competitor.0, "Bee");
        assert_eq!(bee.lap_count(), 1);
    }

    #[test]
    fn render_is_non_empty_and_mentions_pilots() {
        let mut log = InMemoryLog::new();
        let laps = append_and_project(&mut log, &sprint()).unwrap();
        let text = render_lap_list(&laps);
        assert!(text.contains("Ace"));
        assert!(text.contains("Bee"));
    }

    #[test]
    fn format_micros_renders_seconds_and_millis() {
        assert_eq!(format_micros(31_000_000), "31.000 s");
        assert_eq!(format_micros(30_500_000), "30.500 s");
    }
}
