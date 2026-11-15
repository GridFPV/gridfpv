<script lang="ts">
  /**
   * MarshalVideo — a race's video on the Marshal page, lined up with the race clock.
   *
   * It plays the video with the overlay in it when there is one, even when that overlay's
   * laps are out of date: the clock drawn on it is what a marshal reads a crossing against.
   * A clean recording can be picked instead.
   *
   * `playhead` is where the video is, in microseconds into the race (negative before the
   * start). The page hands it to each seat's signal trace, which draws a marker there. The
   * other way, `seek()` moves the video to a moment of the race: a click on a trace calls it.
   *
   * The player's own controls are kept (play, the bar, full screen). Under them are the
   * things a marshal needs and a plain player lacks: slow speeds, and a step of one frame.
   */
  import { Badge, Button } from '@gridfpv/components';
  import type { RaceView } from '@gridfpv/types';
  import {
    FRAME_SECONDS,
    SPEEDS,
    formatRaceMoment,
    raceMicrosAt,
    startInVideoMicros,
    videoChoices,
    videoSecondsAt,
    type WhichVideo
  } from '../lib/marshalVideo.js';

  let {
    race,
    urlOf,
    playhead = $bindable(undefined)
  }: {
    /** The race under review. It has a video, or this is not drawn. */
    race: RaceView;
    /** Where one of the race's videos plays from. */
    urlOf: (which: WhichVideo) => string;
    /** Where the video is, in microseconds into the race. */
    playhead?: number | undefined;
  } = $props();

  const choices = $derived(race.video ? videoChoices(race.video) : []);
  /** The marshal's pick. Until they pick, the first choice: the one with the overlay. */
  let picked = $state<WhichVideo | undefined>(undefined);
  const choice = $derived(choices.find((c) => c.which === picked) ?? choices[0]);
  const startInVideo = $derived(choice ? startInVideoMicros(race, choice.which) : undefined);

  let player = $state<HTMLVideoElement>();
  let speed = $state(1);
  /** The player said it cannot play this file. */
  let unplayable = $state(false);
  let playing = $state(false);

  function read(): void {
    if (!player || startInVideo === undefined) return;
    playhead = raceMicrosAt(player.currentTime, startInVideo);
  }

  /** Move the video to a moment of the race. */
  export function seek(raceMicros: number): void {
    if (!player || startInVideo === undefined) return;
    player.currentTime = videoSecondsAt(raceMicros, startInVideo);
    read();
  }

  function step(frames: number): void {
    if (!player) return;
    player.pause();
    player.currentTime = Math.max(0, player.currentTime + frames * FRAME_SECONDS);
    read();
  }

  function setSpeed(next: number): void {
    speed = next;
    if (player) player.playbackRate = next;
  }

  // While it plays, the marker on the traces follows every frame the screen draws. The
  // player's own `timeupdate` comes only a few times a second, which would make it jump.
  $effect(() => {
    if (!playing) return;
    let frame = requestAnimationFrame(function tick() {
      read();
      frame = requestAnimationFrame(tick);
    });
    return () => cancelAnimationFrame(frame);
  });

  // Another video of the same race keeps the moment of the race the marshal was at. The
  // two files begin at different instants, so it is the race's moment that is kept.
  let keep: number | undefined;
  function pick(which: WhichVideo): void {
    keep = playhead;
    unplayable = false;
    picked = which;
  }
  function loaded(): void {
    if (!player) return;
    player.playbackRate = speed;
    if (keep !== undefined && startInVideo !== undefined) {
      player.currentTime = videoSecondsAt(keep, startInVideo);
      keep = undefined;
    } else if (startInVideo !== undefined) {
      // Open on the start of the race, not on the seconds of waiting before it.
      player.currentTime = videoSecondsAt(0, startInVideo);
    }
    read();
  }

  // The marker goes when the player does.
  $effect(() => () => (playhead = undefined));
</script>

{#if choice}
  <section class="video" aria-label="Video of the race">
    <div class="bar">
      {#if choices.length > 1}
        <div class="pick" role="radiogroup" aria-label="Which video">
          {#each choices as option (option.which)}
            <button
              type="button"
              class="opt"
              role="radio"
              aria-checked={option.which === choice.which}
              onclick={() => pick(option.which)}
            >
              {option.label}
            </button>
          {/each}
        </div>
      {:else}
        <span class="one">{choice.label}</span>
      {/if}
      {#if choice.outOfDate}
        <span data-testid="video-out-of-date">
          <Badge tone="warn">Laps changed since</Badge>
        </span>
        <span class="muted">The laps on it are old. Its clock is still right.</span>
      {/if}
    </div>

    {#if unplayable}
      <p class="problem" role="alert">
        This video cannot be played here. It may have been moved, or it is in a format the app
        cannot play (MP4 works). The file is <code>{choice.file}</code>.
      </p>
    {/if}
    {#key choice.which}
      <!-- The race's own recording: there is no spoken track to caption. -->
      <!-- svelte-ignore a11y_media_has_caption -->
      <video
        bind:this={player}
        class:hidden={unplayable}
        src={urlOf(choice.which)}
        controls
        preload="metadata"
        playsinline
        aria-label="Video of the race, {choice.label.toLowerCase()}"
        onloadedmetadata={loaded}
        ontimeupdate={read}
        onseeked={read}
        onplay={() => (playing = true)}
        onpause={() => {
          playing = false;
          read();
        }}
        onended={() => (playing = false)}
        onerror={() => (unplayable = true)}
      ></video>
    {/key}

    <div class="tools" role="group" aria-label="Video controls">
      <Button size="sm" variant="ghost" aria-label="Back one frame" onclick={() => step(-1)}>
        ◀ Frame
      </Button>
      <Button size="sm" variant="ghost" aria-label="Forward one frame" onclick={() => step(1)}>
        Frame ▶
      </Button>
      <div class="pick" role="radiogroup" aria-label="Speed">
        {#each SPEEDS as option (option)}
          <button
            type="button"
            class="opt"
            role="radio"
            aria-checked={speed === option}
            onclick={() => setSpeed(option)}
          >
            {option}×
          </button>
        {/each}
      </div>
      {#if startInVideo !== undefined}
        <Button size="sm" variant="ghost" onclick={() => seek(0)}>To the start</Button>
      {/if}
      <span class="moment" data-testid="video-moment">
        {#if playhead !== undefined}
          {formatRaceMoment(playhead)} into the race
        {:else if startInVideo === undefined}
          This race has no start to line the video up with.
        {/if}
      </span>
    </div>
  </section>
{/if}

<style>
  .video {
    display: grid;
    gap: var(--gf-space-2);
    min-width: 0;
  }
  video {
    display: block;
    width: 100%;
    max-height: 46vh;
    background: #000;
    border: 1px solid var(--gf-border);
    border-radius: var(--gf-radius-sm);
  }
  video.hidden {
    display: none;
  }
  .bar,
  .tools {
    display: flex;
    align-items: center;
    gap: var(--gf-space-2);
    flex-wrap: wrap;
  }
  .pick {
    display: inline-flex;
    gap: 2px;
    padding: 2px;
    border-radius: var(--gf-radius-sm);
    border: 1px solid var(--gf-border);
    background: var(--gf-surface-sunken);
  }
  .opt {
    appearance: none;
    border: 0;
    border-radius: calc(var(--gf-radius-sm) - 2px);
    padding: var(--gf-space-1) var(--gf-space-3);
    background: transparent;
    color: var(--gf-text-muted);
    font: inherit;
    font-size: var(--gf-font-size-sm);
    cursor: pointer;
  }
  .opt[aria-checked='true'] {
    background: var(--gf-elevated);
    color: var(--gf-text);
    font-weight: var(--gf-font-weight-semibold);
  }
  .opt:focus-visible {
    outline: 2px solid var(--gf-accent);
    outline-offset: 1px;
  }
  .one,
  .muted {
    color: var(--gf-text-muted);
    font-size: var(--gf-font-size-sm);
  }
  .moment {
    margin-left: auto;
    font-family: var(--gf-font-mono);
    font-variant-numeric: tabular-nums;
    font-size: var(--gf-font-size-sm);
    color: var(--gf-text-muted);
  }
  .problem {
    margin: 0;
    color: var(--gf-danger);
    font-size: var(--gf-font-size-sm);
    overflow-wrap: anywhere;
  }
</style>
