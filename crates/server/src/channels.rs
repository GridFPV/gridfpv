//! The standard **FPV channel catalog** (race redesign Slice 4a) — the shared vocabulary of
//! common analog (and clean digital) video channels mapped to their raw centre frequency in
//! **MHz**.
//!
//! Channels in FPV are spoken of by *band + channel label* (e.g. "Raceband R1", "Fatshark F4",
//! "Boscam A8"), but the engine — and every timer — only deals in the raw centre **frequency**
//! ([`gridfpv_engine::schedule::Frequency`], raw MHz). This module is the lookup table between the
//! two: a single, generic, *not* RotorHazard-specific reference the whole system shares.
//!
//! - The **UI** (Slice 4b) reads [`catalog`] (exported as the `ChannelCatalogEntry[]` binding) to
//!   offer the human-readable band/channel labels when a Race Director configures a timer's
//!   available channels.
//! - A **timer adapter** declares its [`channel_capability`](crate::timers::ChannelCapability) in
//!   terms of these raw frequencies; a limited (Fixed) timer exposes only the catalog entries it
//!   physically supports, a Flexible timer the whole catalog plus arbitrary custom MHz.
//! - **Per-heat assignment** ([`crate::round_engine`]) allocates these raw frequencies onto a
//!   heat's lineup with the engine's [`allocate`](gridfpv_engine::schedule::allocate).
//!
//! # The bands (centre frequencies, MHz)
//!
//! - **Raceband** R1–R8 — `5658, 5695, 5732, 5769, 5806, 5843, 5880, 5917`. The de-facto racing
//!   default (mirrors [`FrequencyPool::raceband`](gridfpv_engine::schedule::FrequencyPool::raceband)).
//! - **Fatshark / ImmersionRC (IRC)** F1–F8 — `5740, 5760, 5780, 5800, 5820, 5840, 5860, 5880`.
//! - **Boscam A** A1–A8 — `5865, 5845, 5825, 5805, 5785, 5765, 5745, 5725` (descending, as labelled).
//! - **Boscam B** B1–B8 — `5733, 5752, 5771, 5790, 5809, 5828, 5847, 5866`.
//! - **Boscam E** E1–E8 — `5705, 5685, 5665, 5645, 5885, 5905, 5925, 5945`.
//! - **DJI** (digital, O3/O4-class analog-compatible centres) — the four clean DJI race channels
//!   `5660, 5695, 5735, 5770` (DJI R1–R4 overlap Raceband but are exposed under their own band so a
//!   DJI HD pilot picks a DJI label).
//! - **HDZero** (digital) — `5658, 5695, 5732, 5769, 5806, 5843, 5880, 5917` (HDZero races on the
//!   Raceband grid; exposed under its own band label for a HDZero pilot).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One entry in the standard FPV channel catalog: a band, its channel label, and the raw centre
/// frequency in **MHz** (race redesign Slice 4a).
///
/// The `band`/`channel` pair is the human-readable handle a console offers; `mhz` is the
/// authoritative value every timer and the engine actually tune/allocate on. Exported via
/// `ts_rs::TS` so the frontend can render the catalog directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ChannelCatalogEntry {
    /// The band name (e.g. `"Raceband"`, `"Fatshark"`, `"Boscam A"`, `"DJI"`, `"HDZero"`).
    pub band: String,
    /// The channel label within the band (e.g. `"R1"`, `"F4"`, `"A8"`).
    pub channel: String,
    /// The channel's centre frequency in megahertz (the raw value the engine/timer use).
    pub mhz: u16,
}

impl ChannelCatalogEntry {
    /// A catalog entry from its band, channel label, and centre frequency.
    fn new(band: &str, channel: &str, mhz: u16) -> Self {
        Self {
            band: band.to_string(),
            channel: channel.to_string(),
            mhz,
        }
    }
}

/// The eight Raceband centre frequencies R1–R8, in channel order — the de-facto racing default.
pub const RACEBAND_MHZ: [u16; 8] = [5658, 5695, 5732, 5769, 5806, 5843, 5880, 5917];

/// One labelled band: its name and the eight (label, MHz) channels, in channel order.
fn band(name: &str, channels: [(&'static str, u16); 8]) -> Vec<ChannelCatalogEntry> {
    channels
        .into_iter()
        .map(|(label, mhz)| ChannelCatalogEntry::new(name, label, mhz))
        .collect()
}

/// The full standard FPV channel catalog (race redesign Slice 4a), in a stable, deterministic
/// order: Raceband, Fatshark/IRC, Boscam A/B/E, then the clean digital DJI and HDZero bands.
///
/// This is the shared vocabulary the UI offers and a timer's available channels are picked from.
/// The order is fixed so the binding and any consumer is deterministic (the gen-drift check and
/// the catalog-sanity test both rely on it).
pub fn catalog() -> Vec<ChannelCatalogEntry> {
    let mut out = Vec::new();
    out.extend(band(
        "Raceband",
        [
            ("R1", 5658),
            ("R2", 5695),
            ("R3", 5732),
            ("R4", 5769),
            ("R5", 5806),
            ("R6", 5843),
            ("R7", 5880),
            ("R8", 5917),
        ],
    ));
    out.extend(band(
        "Fatshark",
        [
            ("F1", 5740),
            ("F2", 5760),
            ("F3", 5780),
            ("F4", 5800),
            ("F5", 5820),
            ("F6", 5840),
            ("F7", 5860),
            ("F8", 5880),
        ],
    ));
    out.extend(band(
        "Boscam A",
        [
            ("A1", 5865),
            ("A2", 5845),
            ("A3", 5825),
            ("A4", 5805),
            ("A5", 5785),
            ("A6", 5765),
            ("A7", 5745),
            ("A8", 5725),
        ],
    ));
    out.extend(band(
        "Boscam B",
        [
            ("B1", 5733),
            ("B2", 5752),
            ("B3", 5771),
            ("B4", 5790),
            ("B5", 5809),
            ("B6", 5828),
            ("B7", 5847),
            ("B8", 5866),
        ],
    ));
    out.extend(band(
        "Boscam E",
        [
            ("E1", 5705),
            ("E2", 5685),
            ("E3", 5665),
            ("E4", 5645),
            ("E5", 5885),
            ("E6", 5905),
            ("E7", 5925),
            ("E8", 5945),
        ],
    ));
    // Clean digital bands (exposed under their own labels so a DJI/HDZero pilot picks a DJI/HDZero
    // channel, even where the centre frequency coincides with an analog grid).
    out.extend(
        [("R1", 5660u16), ("R2", 5695), ("R3", 5735), ("R4", 5770)]
            .into_iter()
            .map(|(label, mhz)| ChannelCatalogEntry::new("DJI", label, mhz)),
    );
    out.extend(band(
        "HDZero",
        [
            ("R1", 5658),
            ("R2", 5695),
            ("R3", 5732),
            ("R4", 5769),
            ("R5", 5806),
            ("R6", 5843),
            ("R7", 5880),
            ("R8", 5917),
        ],
    ));
    out
}

/// Whether `mhz` is a frequency the standard catalog knows (any band/channel maps to it). Used to
/// label a raw frequency, and as a sanity gate when a Fixed timer's allowed set is validated.
pub fn is_known(mhz: u16) -> bool {
    catalog().iter().any(|e| e.mhz == mhz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raceband_is_r1_through_r8_in_order() {
        let race: Vec<_> = catalog()
            .into_iter()
            .filter(|e| e.band == "Raceband")
            .collect();
        assert_eq!(race.len(), 8);
        assert_eq!(race[0].channel, "R1");
        assert_eq!(race[0].mhz, 5658);
        assert_eq!(race[7].channel, "R8");
        assert_eq!(race[7].mhz, 5917);
        // The const mirrors the catalog's Raceband row exactly.
        let mhz: Vec<u16> = race.iter().map(|e| e.mhz).collect();
        assert_eq!(mhz, RACEBAND_MHZ);
    }

    #[test]
    fn catalog_covers_every_required_band() {
        let bands: std::collections::BTreeSet<String> =
            catalog().into_iter().map(|e| e.band).collect();
        for required in ["Raceband", "Fatshark", "Boscam A", "Boscam B", "Boscam E"] {
            assert!(
                bands.contains(required),
                "catalog missing band {required:?}"
            );
        }
        // The clean digital bands are present too.
        assert!(bands.contains("DJI"));
        assert!(bands.contains("HDZero"));
    }

    #[test]
    fn every_entry_is_a_plausible_58ghz_channel() {
        for entry in catalog() {
            assert!(
                (5600..=6000).contains(&entry.mhz),
                "{} {} = {} MHz is outside the 5.8 GHz band",
                entry.band,
                entry.channel,
                entry.mhz
            );
            assert!(!entry.channel.is_empty());
        }
    }

    #[test]
    fn is_known_recognises_catalog_frequencies_only() {
        assert!(is_known(5658)); // Raceband R1
        assert!(is_known(5800)); // Fatshark F4
        assert!(!is_known(1234)); // not an FPV channel
    }
}
