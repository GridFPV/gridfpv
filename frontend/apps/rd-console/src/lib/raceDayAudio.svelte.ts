/**
 * **App-wide race-day audio** — the tones + spoken lap callouts, hoisted OUT of the Live Race
 * Control page so a running race is audible on EVERY page (marshaling, rounds, results, …).
 * The RD stages a heat and walks over to the rounds screen; the start tone, crossing pips,
 * callouts, end-of-race countdown, and buzzer must not care where the console is looking.
 *
 * One controller per console, mounted ONCE from `App.svelte` (component context — its
 * `$effect`s need an owner) and shared with the pages through {@link raceDayAudio}. The Live
 * page keeps its Callouts toggle + control-click unlocks; they drive this shared instance.
 *
 * Everything here consumes the app-wide `session.liveState` (the same stream the global
 * header's clock uses on every in-event page) plus the open directory reads (heats, pilots,
 * channels) it needs to resolve callsigns and the current round's tone/window config. The
 * detection/scheduling logic itself is unchanged — `lapCallouts.svelte.ts` and
 * `endTones.svelte.ts` are pure and were already page-agnostic; only their owner moved.
 */
import type {
  ChannelCatalogEntry,
  CompetitorRef,
  HeatId,
  HeatSummary,
  Pilot,
  RoundDef
} from '@gridfpv/types';
import type { Session } from './session.svelte.js';
import { RaceAudioPlayer } from './raceAudio.js';
import { CalloutQueue, lapCalloutText } from './callouts.js';
import { useEndOfRaceTones } from './endTones.svelte.js';
import { useCrossingTones, useLapCallouts } from './lapCallouts.svelte.js';
import { buildCompetitorNames } from './competitorName.js';
import { fixedEndWindowMicros } from './raceWindow.js';

/** The shared controller surface the pages use (the Live page's Callouts toggle). */
export interface RaceDayAudio {
  /** Whether the informational layer (pips + speech) is muted. Reactive. */
  readonly muted: boolean;
  /** Toggle the informational mute (persisted); returns the new state. */
  toggleMuted(): boolean;
  /** Resume/unlock the audio context — call from any RD gesture (Stage/Start clicks). */
  resume(): void;
}

let controller: RaceDayAudio | undefined;

/** A silent stand-in for contexts with no mounted controller (component unit tests render
 *  pages without the App shell; the real console always mounts in App.svelte). */
const NOOP: RaceDayAudio = {
  muted: false,
  toggleMuted: () => false,
  resume: () => {}
};

/** The mounted app-wide controller — or a silent no-op before/without `mountRaceDayAudio`. */
export function raceDayAudio(): RaceDayAudio {
  return controller ?? NOOP;
}

/**
 * Mount the app-wide audio controller. Call ONCE from `App.svelte`'s component setup — the
 * internal `$effect`s bind to that component context and live for the app's lifetime.
 */
export function mountRaceDayAudio(session: Session): RaceDayAudio {
  const audio = new RaceAudioPlayer();
  const callouts = new CalloutQueue();
  $effect(() => () => {
    callouts.cancel();
    audio.dispose();
  });

  // Autoplay unlock + speech warm-up: the RD's FIRST gesture anywhere resumes the
  // AudioContext AND spins the TTS engine up (its first-ever utterance can take seconds — or
  // die silently — while the backend initializes; warming here means the first real lap
  // callout speaks promptly instead of ~10s into the race).
  $effect(() => {
    const unlock = () => {
      void audio.resume();
      callouts.warmUp();
    };
    document.addEventListener('pointerdown', unlock, { once: true });
    return () => document.removeEventListener('pointerdown', unlock);
  });

  // ── The live essentials, straight off the app-wide stream ─────────────────────────────────
  const live = $derived(session.liveState);
  const phase = $derived(live?.phase);
  const heat = $derived(live?.current_heat);

  // ── Directory reads (heats / pilots / channels), refreshed as the stream advances ─────────
  // The same open-read pattern the Live page uses; errors leave the previous value standing
  // (a resolver miss falls back to the raw ref, and the callout then SKIPS the name rather
  // than speaking an id — the friendly-names rule).
  let heats = $state<HeatSummary[]>([]);
  let pilots = $state<Pilot[]>([]);
  let catalog = $state<ChannelCatalogEntry[]>([]);
  $effect(() => {
    void session.protocolState;
    session
      .listHeats()
      .then((h) => (heats = h))
      .catch(() => {});
    session
      .listPilots()
      .then((p) => (pilots = p))
      .catch(() => {});
  });
  $effect(() => {
    void session.currentEvent?.id;
    session
      .listChannels()
      .then((c) => (catalog = c))
      .catch(() => {});
  });

  // The current heat's round: the start-tone cue + the fixed-end window both live on it.
  const currentRound = $derived.by<RoundDef | undefined>(() => {
    const summary = heats.find((h) => h.heat === heat);
    const roundId = summary?.round;
    if (!roundId) return undefined;
    return session.currentEvent?.rounds?.find((r) => r.id === roundId);
  });
  const toneCue = $derived(currentRound?.start_procedure?.tone);
  const windowMicros = $derived(fixedEndWindowMicros(currentRound));

  // The shared callsign resolver (friendly-names rule) — roster binding first, explicit register
  // second, the `"Node 7 · Raceband R7"` seat label for a bare node seat, raw ref last. Built from
  // the ONE shared input assembly every screen uses (#416).
  const names = $derived(
    buildCompetitorNames({
      pilots,
      progress: live?.progress,
      heat: heats.find((h) => h.heat === heat),
      catalog,
      timer: session.primaryTimer,
      membership: session.currentEvent?.classes_membership
    })
  );
  const competitorName = $derived.by<(ref: CompetitorRef) => string>(() => names.name);

  // ── Start tone: fire on an OBSERVED transition into Running, never a late join ────────────
  // (Unchanged logic — see the original Live-page comment block. Hoisted here, "late join"
  // now means the console APP loading onto an in-progress race, not a page navigation: moving
  // between pages no longer remounts this controller, so navigating back to Live mid-race
  // can never re-buzz.)
  let toneFiredForHeat = $state<HeatId | undefined>(undefined);
  let tonePreRunningForHeat = $state<HeatId | undefined>(undefined);
  $effect(() => {
    const p = phase;
    const h = heat;
    if (h === undefined) return;
    if (p !== 'Running') {
      if (p === 'Scheduled' || p === 'Staged' || p === 'Armed') tonePreRunningForHeat = h;
      if (toneFiredForHeat !== h) toneFiredForHeat = undefined;
      return;
    }
    if (toneFiredForHeat !== h && tonePreRunningForHeat === h) {
      toneFiredForHeat = h;
      audio.playStartTone(toneCue);
    }
  });

  // ── End-of-race countdown + buzzer (procedure tones — always on) ──────────────────────────
  useEndOfRaceTones(
    () => phase,
    () => heat,
    () => live?.race_started_at,
    () => windowMicros,
    () => session.serverNowMs(),
    {
      onCountdown: () => audio.playCountdownBeep(),
      onRaceEnd: () => audio.playRaceEndTone()
    }
  );

  // ── Crossing tones (informational layer — mute-scoped) ────────────────────────────────────
  // The TONE half, and the whole of #397: one pip per gate CROSSING — holeshot, counted lap, and
  // a pass the min-lap floor rejected alike — where the console used to pip only per recorded
  // *lap* and so was silent for exactly the crossings an RD most needs to hear. A pip on a seat
  // nobody is flying is the point, not a bug: that is how a too-sensitive gate becomes audible.
  //
  // Novelty is the crossing's `pass_ref`, never the arrival of a frame (see the detector); the
  // player self-gates on the callouts mute, so the mute has one owner and cannot drift.
  useCrossingTones(
    () => session.currentEvent?.id,
    () => phase,
    () => live?.crossings,
    () => audio.playCrossingBeep()
  );

  // ── Spoken lap callouts (informational layer — mute-scoped) ────────────────────────────────
  // The VOICE half, still driven by recorded laps: a lap number and a lap time are the payload
  // of a callout, and a holeshot or a rejected pass has neither to say. So the tone fires per
  // crossing and the speech per lap — no crossing is both pipped twice and no lap is spoken
  // twice. (The pip that used to fire from here moved to the crossing feed above; leaving it
  // would double-pip every counted lap.)
  useLapCallouts(
    () => phase,
    () => heat,
    () => live?.race_started_at,
    () => live?.progress,
    (crossing) => {
      if (audio.muted) return;
      // Spoken, not printed: a seat label carries its channel after a "·" separator, which reads
      // as punctuation on screen and as noise out loud — so the callout speaks the node alone.
      // A ref that resolved to nothing but itself is still skipped rather than spelled out.
      const resolved = competitorName(crossing.ref);
      const name = resolved === crossing.ref ? undefined : resolved.split(' · ')[0];
      callouts.enqueue({
        text: lapCalloutText(name, crossing.lap, crossing.lastLapMicros),
        key: crossing.ref
      });
    }
  );
  // A natural race end drains the queue; only a NEW run taking the stage drops the backlog.
  $effect(() => {
    if (phase === 'Scheduled' || phase === 'Staged' || phase === 'Armed') callouts.cancel();
  });

  let muted = $state(audio.muted);
  controller = {
    get muted() {
      return muted;
    },
    toggleMuted() {
      void audio.resume();
      callouts.warmUp();
      muted = audio.toggleMuted();
      if (muted) callouts.cancel();
      return muted;
    },
    resume() {
      void audio.resume();
      callouts.warmUp();
    }
  };
  return controller;
}
