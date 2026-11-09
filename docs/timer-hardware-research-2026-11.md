# Timer hardware research — November 2026

> **Under review (2026-11-09).** The hardware research stands. The "GridNode" protocol and
> adapter it sketches are replaced by OFT (Open FPV Timer). See [Grid primitives](primitives.md).

**Status:** research and direction only. Nothing is built and nothing is decided.
Written 2026-11-08 from a working session with Ryan.

**Why:** RX5808 (RTC6715) modules are hard to buy. Separately, we want (a) a cheap,
accessible single-pilot gate timer and (b) tunable, more accurate multi-pilot (8+) nodes
for Grid.

**Current lean:** use the **ESP32-C5 as a software-defined receiver** for both the
single-node timer and the multi-pilot nodes, wired back to Grid over **Ethernet**.

---

## 1. What the RX5808 actually gives RotorHazard

- RotorHazard uses the RTC6715 only for SPI tuning and its analog RSSI pin. That pin
  measures in-channel power over a window of about 25–30 MHz, is roughly logarithmic,
  compresses around −40 dBm, and is slowed by an RC filter.
- All crossing logic lives in node firmware (median and smoothing filter, enter/exit
  thresholds, reports the peak time) and the server (calibration, RSSI history for the
  Marshal view).
- The RX5808 antenna pad is a 50 Ω input that RotorHazard builds usually leave floating.

## 2. Gate geometry (Ryan's setup)

- Gate opening is 24" × 24" (0.61 m), made from 1/2" or 3/4" Sch40 PVC
  (OD 21.3 mm / 26.7 mm).
- Pilots fly analog and HDZero, generally at 25 mW.
- The antenna sits on the top bar and looks down. The quad passes about 0.3 m from it
  (0.05 m at worst, if it clips the top bar).

Link budget at 25 mW (14 dBm VTX, quad antenna about 2 dBi, 5.8 GHz):

| Pass | 0 dBi rx | 6 dBi PCB patch | 8.4 dBi patch |
|---|---|---|---|
| 0.3 m (mid gate) | ≈ −21 dBm | ≈ −15 dBm | ≈ −13 dBm |
| 0.05 m (clips top bar) | ≈ −5 dBm | ≈ +1 dBm | ≈ +3 dBm |
| 5 m in front | ≈ −45 dBm | ≈ −39 dBm | ≈ −37 dBm |

Takeaways:

- On a small gate, distance does most of the work, so high antenna gain is unnecessary
  and slightly harmful.
- The antenna's job is to **reject quads above and beside the gate**. A ground plane
  facing up does that. A 70–90° beam looking down covers the opening.
- Polarisation barely matters here: a linear antenna against a circularly polarised quad
  antenna loses only 3 dB.
- Time crossings from the **midpoint between the half-maximum edges** of the pulse, not
  the peak. The quad antenna's axial null can dip the middle of the pulse. Expected
  result is laps repeatable to a few ms rather than tens of ms.
- A pass lasts about 30–40 ms at 15 m/s.

## 3. Antenna options (cheap first)

1. **Stubby you already own, inside a foil-lined 3D-printed cup.** About $0–3, fastest to
   test.
2. **PCB patch etched on FR4** (1.6 mm, εr about 4.4): about 15.7 × 11.7 mm with an inset
   feed, on a ground plane of at least 40 × 40 mm. About 5–6 dBi and an 80–90° beam,
   with 3–4% bandwidth (R1/R8 lose a couple of dB). εr tolerance shifts the resonance by
   ±3%, so put length variants on the first panel. Tune by sweeping a VTX through the
   channels and reading the detector, so no VNA is needed. A LiteVNA (to 6.3 GHz) is
   optional; the NanoVNA-H4 only reaches 1.5 GHz.
3. Branded circularly polarised patches (Lumenier AXII 8.4 dBic/76°, SpiroNET 8 dBi/90°).
   These work but are not needed for this gate.

Two patches 8–10 cm apart along the flight line give a pulse offset of about 6 ms,
which gives **pass direction** (issue #106).

## 4. Receiver options considered

| Option | Notes |
|---|---|
| **AD8318 log detector** + Pico | Measures total power and doesn't care about channels (works with analog, HDZero, DJI and Walksnail), but can't separate pilots, so it suits single-pilot only. Range about −60 to −2 dBm, −25 mV/dB, 5 V at 68 mA. LCSC C578694 costs about $8–9; AD8317 (C578693) is the same price in a smaller package. Cheap breakouts may carry relabelled chips. |
| **RX5808 + long coax to a gate antenna** | Works today with existing nodes. 10–15 m of RG174 (≈ 2 dB/m) doubles as the 30–40 dB attenuator the RX5808 needs. The module and node box still pick up signal directly, so shield them. A 4–8 way splitter lets one gate antenna feed several nodes (multi-pilot). Doesn't solve RX5808 supply. Good **phase 0** test of the antenna idea. |
| **ESP32-C5 software radio** | **Preferred.** See §5. |
| Discrete superheterodyne per pilot (PLL, mixer, IF SAW, log detector, digital step attenuator) | LapRF-class quality. Costs about $30–50 per channel plus a lot of RF layout. Only worth it if the C5 hits an accuracy ceiling. |
| Wideband digitizer covering the whole band, with FPGA channelization | Needs 300 MHz of instantaneous bandwidth, so $500+. Pluto/HackRF-class SDRs would have to hop anyway. |
| More RTC6715 sources | RC832 ground receivers, FR632 boards, module-bay receivers, dead analog goggles. Same as today. |

**ImmersionRC LapRF:** the internals are unpublished, and no teardown or FCC internal
photos were found. Its published specs mention a custom, calibrated receiver module,
5.3–6.0 GHz, **programmable 0–32 dB attenuation**, heavy filtering, and **custom antennas
with gate-shaped beams**. Our inference is a superheterodyne receiver per pilot with a
digital step attenuator in front. Their emphasis on antennas agrees with §2.

## 5. ESP32-C5 as the receiver

The C5 is the only ESP32 with a 5 GHz radio. C3, C6 and S3 can't do this.

- **FPVGateC5RX** (single channel): uses undocumented `phy_check_sigrssi_en` /
  `phy_get_sigrssi` polled at about 1 kHz, plus `RFChannelSel` for direct tuning. It
  emulates an RX5808 (3-wire SPI in, sigma-delta RSSI voltage out). On the bench it reads
  −12 to −24 dBm at 50 cm on channel versus −93 to −97 off channel, with 7 ms rise and
  30 ms fall, usable to about 5960 MHz. Licence CC BY-NC-SA 4.0.
- **FPVGateC5MK** (multi-pilot): one C5 hops across up to 8 pilots at **about 1,030
  readings per pilot per second**, measuring band power over about 10 MHz from raw I/Q.
  Covers 5180–5917 MHz (R1–R8; Boscam E7/E8 out of range). Appears to RotorHazard as
  **8 standard nodes over USB** and timestamps on the C5. Known issues: image rejection,
  spikes just after each hop, and no lap-accuracy measurement against video yet.
  Licence CC BY-NC-SA 4.0, commercial use by separate licence.
- **widscan58** (CapnBry, built on esp-sdr): per-channel RSSI, noise floor, SNR, measured
  centre frequency and 99% occupied bandwidth, plus a band panorama. Monitor mode updates
  at about 50–60 Hz per channel. Can act as an 8-channel RX5808 array for RotorHazard.
  **Licence GPL-3.0-or-later, which can be combined with Grid's AGPL-3.0, so it is the base
  to build on.** FPVGate's NC licence cannot be combined with AGPL.

Why the C5 beats the RX5808:

- One $6–8 chip covers 8 pilots.
- I/Q samples give the noise floor and SNR, so thresholds can be set automatically
  relative to the noise floor instead of by manual Enter/Exit calibration.
- It can detect a pilot who is off-channel or off-frequency.
- Filter bandwidth is set in firmware rather than by a slow RC filter.

Risks:

- Undocumented PHY and debug paths, so pin the ESP-IDF version.
- Spikes after each hop need filtering.
- Strong signals near the gate need attenuation (a digital step attenuator or splitter
  loss).
- The C5 is single-core, so its CPU is shared between sampling and comms.
- No accuracy data against a reference yet. We would be generating it.

## 6. Talking back to Grid over Ethernet

The C5 has no Ethernet MAC, but ESP-IDF's `esp_eth` drives SPI Ethernet chips (W5500 and
others) on any ESP32. Wi-Fi is not an option while the radio is busy sampling, and Ryan
prefers wired anyway.

### Topologies

**A. Hub (recommended for a gate box).** One PoE board holds an aggregator MCU (Pico 2 /
RP2350 + W5500, or an Ethernet-capable ESP32-P4) and 1–N C5 radio modules connected by UART
(RP2350 PIO can add UARTs), with a **shared SYNC line**.

- The hub pulses SYNC (e.g. 10–100 Hz). Every C5 timestamps the edge, so the hub maps all
  C5 timestamps onto **one clock**.
- The hub speaks to Grid as **one source**. One Ethernet cable, many C5s.
- Single-node timer = hub + 1 C5. 8-pilot node = hub + 2–4 C5s, giving 2–4 pilots per C5
  and longer dwell per channel.
- The hub could also host a "small Grid" for standalone use.

**B. Direct (for distributed gates).** Each C5 has its own W5500 and PoE and plugs into a
PoE switch.

- Each node is its own Grid source with its own clock.
- This is fine for laps, because a lap is an interval between two crossings of one pilot at
  one node (§7).
- It suits split gates spread around a track.

Both topologies should speak **the same wire protocol**, so Grid needs one adapter.

### Wire protocol sketch ("GridNode")

- **Discovery:** mDNS `_gridnode._tcp`. The node listens; Grid connects (same direction
  as RotorHazard).
- **Transport:** TCP with length-prefixed frames. Small fixed little-endian structs for
  high-rate samples, and CBOR for control messages. Keep it language-neutral, since C5
  firmware is C/ESP-IDF while the hub and Grid are Rust. Version it from day one.
- **Messages:**
  - `Hello`: node id, firmware, protocol version, radios and seats, tunable range, clock
    rate.
  - `Config`: seat → frequency, attenuation/gain, threshold mode (auto relative to SNR, or
    manual enter/exit).
  - `Samples`: seat, t0, dt, `[power, noise]`, batched at about 20–50 Hz. 8 seats × 1 kHz ×
    4 B is about 32 kB/s, which is trivial on Ethernet.
  - `Crossing`: seat, sequence number, t_enter, t_exit, t_mid (the half-maximum midpoint,
    which is the lap time), peak, SNR.
  - `ClockSync`: ping/pong for session alignment.
  - `Health`: temperature, hop errors, dropped samples, link state.

### Fit with Grid's adapter model (`crates/adapters`)

- Grid would get a new `gridnode` adapter with capabilities **LivePasses, SignalContext,
  Calibration, SignalRecovery, FrequencyMgmt**.
- The existing `SignalChunk`, `SignalThresholds` and `SignalHistory` paths already carry
  this kind of data.
- Streaming raw samples, not just crossings, enables SignalRecovery: Grid can re-derive a
  missed lap from history.

## 7. Clock model notes

Grid computes intervals on **source time** and only aligns sources to the session timeline
for ordering and display (`crates/adapters/src/clock.rs`, `docs/timer-adapters.html` §4).

- **Hub topology:** the hub is the single source clock, and SYNC keeps its C5s inside that
  clock domain. This matters if a seat's pilot moves between C5s or the hub combines
  readings from two radios.
- **Direct topology:** one clock per node is fine for laps. Splits across gates need
  alignment that is good to about 1 ms. Software-timestamped NTP or PTP-lite over a quiet
  LAN should do; hardware PTP is not needed.

## 8. Proposed steps

0. **Antenna test with what we already own:** RX5808 node + 10–15 m coax + an antenna on
   the gate (foil cup), against the current omni node. Compare lap scatter.
1. **C5 bench test:** 2–3 Seeed XIAO ESP32-C5 boards (u.FL). Flash FPVGateC5MK and
   widscan58 unchanged and run them in gridfpv-rh as RotorHazard nodes beside the RX5808
   node. Fly over and beside the gate as well as through it.
2. **Network dev:** W5500-EVB-Pico2, a generic 802.3af splitter with 5 V output, an
   injector, and a Raspberry Pi Debug Probe. Bring up Embassy and `embassy-net-wiznet` and
   a first `Hello`/`Samples` exchange.
3. **Grid-native C5 firmware** on the GPL esp-sdr / widscan58 base: SNR-relative
   thresholds, half-maximum midpoint timing, the GridNode protocol over UART to the hub.
4. **`gridnode` adapter** in `crates/adapters`, replay-testable from recorded sample
   streams.
5. **Hub PCB:** PoE (Ag9905M module, about $8–9), W5500 (LCSC C32843, about $2.8), a
   PoE-rated magjack (**not** the HR911105A, whose centre taps aren't brought out for
   PoE), 1–4 C5 module footprints, SYNC bus, PCB patch(es), attenuator, status LED. Target
   is under $50 for the single-node version.

Fallback single-pilot design that needs no C5 (from earlier in the session): Pico 2 +
AD8318 + PCB patch + PoE carrier, about $37–43 at quantity 5.

## Sources

- FPVGateC5RX: https://github.com/LouisHitchcock/FPVGateC5RX
- FPVGateC5MK: https://github.com/LouisHitchcock/FPVGateC5MK
- FPVGate: https://github.com/LouisHitchcock/FPVGate
- widscan58: https://github.com/CapnBry/widscan58
- ImmersionRC LapRF: https://www.immersionrc.com/fpv-products/laprf/
- LapRF 8-Way manual: https://www.manualslib.com/manual/2221648/Immersion-Rc-Laprf-8-Way-Event-Timing.html
- AD8318 datasheet: https://www.alldatasheet.com/datasheet-pdf/pdf/106299/AD/AD8318.html
- AD8318 LCSC: https://www.lcsc.com/product-image/C578694.html
- AD8317 LCSC: https://www.lcsc.com/product-detail/C578693.html
- W5500 LCSC: https://www.lcsc.com/product-image/C32843.html
- WIZnet W5500-EVB-Pico-PoE: https://thepihut.com/products/wiznet-w5500-evb-pico-poe
- WIZPoE-P1: https://docs.wiznet.io/Product/Modules/PoE/WIZPoE-P1
- Silvertel AG9905M: https://www.symmetryelectronics.com/products/silvertel/ag9905m
- Lumenier AXII patch: https://www.getfpv.com/lumenier-axii-patch-antenna-rhcp.html
- ImmersionRC SpiroNET patch: https://www.immersionrc.com/?p=5329
- Homebrew 5.8 GHz patch: https://vk4ghz.com/homebrew-5-8-ghz-patch-fpv-antenna/
