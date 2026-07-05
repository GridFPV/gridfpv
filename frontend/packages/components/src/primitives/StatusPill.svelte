<script lang="ts">
  /**
   * StatusPill — a domain-aware status indicator for the console state machines:
   * heat **phase** (Scheduled→Staged→Armed→Running→Unofficial→Final), the read-stream
   * **connection** status (live/connecting/reconnecting/closed/idle), and a **timer's**
   * connection status (Ready/Configured/Connecting/Connected/Disconnected/Error, #73/#65).
   *
   * Pass `phase` OR `status`; the pill maps it to the matching token color and a
   * leading dot. The active/live states (Running, live, Connected) pulse to read as
   * "alive" across a room. `label` overrides the displayed text (the state value is
   * used by default).
   *
   * The connection and timer status vocabularies don't collide (the read stream uses
   * lowercase `live`/`connecting`; a timer's `TimerStatus` is capitalized `Connected`/
   * `Connecting`/…), so one `status` map serves both.
   */
  type Phase = 'Scheduled' | 'Staged' | 'Armed' | 'Running' | 'Unofficial' | 'Final';

  let {
    phase = undefined,
    status = undefined,
    label = undefined,
    size = 'md'
  }: {
    phase?: Phase | string;
    status?: string;
    label?: string;
    size?: 'sm' | 'md';
  } = $props();

  const PHASE_TONE: Record<string, string> = {
    Scheduled: 'scheduled',
    Staged: 'staged',
    Armed: 'armed',
    Running: 'running',
    Unofficial: 'finished',
    Final: 'scored'
  };
  const CONN_TONE: Record<string, string> = {
    // Read-stream connection status (lowercase).
    live: 'live',
    connecting: 'pending',
    snapshotting: 'pending',
    reconnecting: 'warn',
    closed: 'warn',
    idle: 'idle',
    // Timer connection status (`TimerStatus`, capitalized — #73/#65). A live, healthy
    // timer reads green; while dialing it's pending; dropped/errored is danger; the
    // resting Mock/configured states are calm/neutral so a single Mock never alarms.
    Ready: 'ready',
    Configured: 'configured',
    Connecting: 'pending',
    Connected: 'live',
    Disconnected: 'warn',
    Error: 'danger'
  };

  const kind = $derived(phase !== undefined ? 'phase' : 'conn');
  const value = $derived(phase ?? status ?? '');
  const tone = $derived(
    phase !== undefined
      ? (PHASE_TONE[phase] ?? 'scheduled')
      : (CONN_TONE[status ?? ''] ?? 'pending')
  );
  const pulse = $derived(value === 'Running' || value === 'live' || value === 'Connected');
  const text = $derived(label ?? value);
</script>

<span
  class="gf-pill"
  data-kind={kind}
  data-tone={tone}
  data-size={size}
  class:pulse
  aria-label={`${kind === 'phase' ? 'Phase' : 'Connection'}: ${text}`}
>
  <span class="gf-pill-dot" aria-hidden="true"></span>
  <span class="gf-pill-text">{text}</span>
</span>

<style>
  .gf-pill {
    --_c: var(--gf-phase-scheduled);
    display: inline-flex;
    align-items: center;
    gap: var(--gf-space-2);
    padding: 0.2em 0.7em;
    border-radius: var(--gf-radius-pill);
    background: color-mix(in srgb, var(--_c) 14%, transparent);
    color: color-mix(in srgb, var(--_c) 85%, var(--gf-text));
    border: 1px solid color-mix(in srgb, var(--_c) 35%, transparent);
    font-family: var(--gf-font-family);
    font-size: var(--gf-font-size-xs);
    font-weight: var(--gf-font-weight-semibold);
    letter-spacing: var(--gf-tracking-wide);
    text-transform: uppercase;
    line-height: 1.4;
    white-space: nowrap;
  }
  .gf-pill[data-size='md'] {
    font-size: var(--gf-font-size-sm);
    text-transform: none;
    letter-spacing: var(--gf-tracking-tight);
  }

  .gf-pill[data-tone='scheduled'] {
    --_c: var(--gf-phase-scheduled);
  }
  .gf-pill[data-tone='staged'] {
    --_c: var(--gf-phase-staged);
  }
  .gf-pill[data-tone='armed'] {
    --_c: var(--gf-phase-armed);
  }
  .gf-pill[data-tone='running'] {
    --_c: var(--gf-phase-running);
  }
  .gf-pill[data-tone='finished'] {
    --_c: var(--gf-phase-finished);
  }
  .gf-pill[data-tone='scored'] {
    --_c: var(--gf-phase-scored);
  }
  .gf-pill[data-tone='live'] {
    --_c: var(--gf-conn-live);
  }
  .gf-pill[data-tone='pending'] {
    --_c: var(--gf-conn-pending);
  }
  .gf-pill[data-tone='warn'] {
    --_c: var(--gf-conn-warn);
  }
  .gf-pill[data-tone='idle'] {
    --_c: var(--gf-conn-idle);
  }
  .gf-pill[data-tone='ready'] {
    --_c: var(--gf-success);
  }
  .gf-pill[data-tone='configured'] {
    --_c: var(--gf-info);
  }
  .gf-pill[data-tone='danger'] {
    --_c: var(--gf-danger);
  }

  .gf-pill-dot {
    width: 0.5em;
    height: 0.5em;
    border-radius: 50%;
    background: var(--_c);
    flex-shrink: 0;
  }
  .gf-pill.pulse .gf-pill-dot {
    box-shadow: 0 0 0 0 var(--_c);
    animation: gf-pulse 1.6s var(--gf-ease-out) infinite;
  }
  @keyframes gf-pulse {
    0% {
      box-shadow: 0 0 0 0 color-mix(in srgb, var(--_c) 70%, transparent);
    }
    70% {
      box-shadow: 0 0 0 0.5em transparent;
    }
    100% {
      box-shadow: 0 0 0 0 transparent;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .gf-pill.pulse .gf-pill-dot {
      animation: none;
    }
  }
</style>
