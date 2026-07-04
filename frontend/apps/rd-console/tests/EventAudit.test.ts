/**
 * The event-wide Audit page: renders the Director's heat-tagged, newest-first audit trail
 * (`GET /events/{id}/audit`) with friendly names everywhere, client-side filters (pilot / heat /
 * kind / free text), clickable pilot+heat chips that set those filters, the #340 error+retry
 * treatment, and the cross-screen prefilter seam (auditFilter) consumed on mount.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor, within } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import type { EventAuditEntry, EventMeta, HeatSummary, RoundDef } from '@gridfpv/types';
import EventAudit from '../src/screens/EventAudit.svelte';
import { consumeAuditPrefilter, openAudit } from '../src/lib/auditFilter.svelte.js';
import { makeTestSession } from './support.js';

// A roster-seeded event: the competitor refs ARE directory pilot ids, so callsigns must resolve
// from the pilots directory alone (the common FromRoster case), never render raw.
const ROUND = {
  id: 'r1',
  label: 'Qualifying R1',
  classes: ['c1'],
  format: 'timed_qual',
  params: {},
  win_condition: { Timed: { window_micros: 120_000_000 } },
  seeding: 'FromRoster',
  channel_mode: 'Static',
  protest_window: 'Off'
} as unknown as RoundDef;
const EVENT: EventMeta = {
  id: 'e1',
  name: 'Friday',
  created_at: 0,
  persistent: true,
  timers: ['mock'],
  roster: [],
  classes: ['c1'],
  rounds: [ROUND]
};
const PILOTS = [
  { id: 'maverick-4d9rp8', callsign: 'Maverick', vtx_types: [] },
  { id: 'goose-yla6dp', callsign: 'Goose', vtx_types: [] }
];
const HEATS: HeatSummary[] = [
  {
    heat: 'q1-heat',
    lineup: ['maverick-4d9rp8', 'goose-yla6dp'],
    round: 'r1',
    class: 'c1',
    frequencies: [],
    phase: 'Final',
    is_current: false
  },
  {
    heat: 'q2-heat',
    lineup: ['maverick-4d9rp8', 'goose-yla6dp'],
    round: 'r1',
    class: 'c1',
    frequencies: [],
    phase: 'Unofficial',
    is_current: true
  }
];

// The server's trail: newest first (descending at_ref), heat-tagged. The reversal (#40) targets
// the DQ (#20) so its pilot resolves by CHASING the chain; the void's target (a pass offset) is
// not an audit entry and the page holds no lap list, so that row renders with no pilot chip.
const ENTRIES: EventAuditEntry[] = [
  {
    heat: 'q1-heat',
    kind: 'RulingReversed',
    at: 1_700_000_400_000_000,
    at_ref: 40,
    competitor: null,
    summary: 'Ruling reversed (ref 20)'
  },
  {
    heat: 'q2-heat',
    kind: 'Voided',
    at: 1_700_000_300_000_000,
    at_ref: 30,
    competitor: null,
    summary: 'Detection voided (ref 25)'
  },
  {
    heat: 'q1-heat',
    kind: 'PenaltyApplied',
    at: 1_700_000_200_000_000,
    at_ref: 20,
    competitor: 'goose-yla6dp',
    summary: 'DQ applied'
  },
  {
    heat: 'q1-heat',
    kind: 'ProtestFiled',
    at: 1_700_000_100_000_000,
    at_ref: 10,
    competitor: 'maverick-4d9rp8',
    summary: 'Protest filed: contact'
  }
];

function renderAudit(overrides: Parameters<typeof makeTestSession>[0] = {}) {
  const made = makeTestSession({
    event: EVENT,
    eventAuditImpl: vi.fn(async () => ENTRIES),
    listHeatsImpl: vi.fn(async () => HEATS),
    listPilotsImpl: vi.fn(async () => PILOTS as unknown as never),
    listChannelsImpl: vi.fn(async () => []),
    ...overrides
  });
  render(EventAudit, { session: made.session });
  return made;
}

const rows = () => within(screen.getByLabelText('Audit entries')).getAllByRole('listitem');

describe('EventAudit — the event-wide audit page', () => {
  it('renders the trail newest-first with kind badges, callsigns, and friendly heat names', async () => {
    renderAudit();
    await waitFor(() => expect(rows()).toHaveLength(4));
    const entries = rows();

    // Newest first, as served (descending at_ref) — the page must not re-order.
    expect(entries[0]).toHaveTextContent('Ruling reversed');
    expect(entries[3]).toHaveTextContent('Protest filed');

    // The PenaltyApplied entry's badge derives the finer 'DQ' from the summary.
    expect(within(entries[2]).getByText('DQ')).toBeInTheDocument();

    // Pilot resolution: structured ref (Goose), and CHASED through the reversal's target (#20 →
    // the DQ → Goose). The heat renders as its friendly "<Round> Heat N" name.
    await waitFor(() => {
      expect(within(entries[0]).getByRole('button', { name: 'Goose' })).toBeInTheDocument();
      expect(within(entries[2]).getByRole('button', { name: 'Goose' })).toBeInTheDocument();
      expect(
        within(entries[0]).getByRole('button', { name: 'Qualifying R1 Heat 1' })
      ).toBeInTheDocument();
      expect(
        within(entries[1]).getByRole('button', { name: 'Qualifying R1 Heat 2' })
      ).toBeInTheDocument();
    });

    // The trailing detail is the entry's own log ref; the raw "(ref N)" clause is stripped from
    // the text (the target renders only as the "· #N" tail, #337).
    expect(within(entries[0]).getByText('#40')).toBeInTheDocument();
    expect(entries[0]).toHaveTextContent('Ruling reversed · #20');
    expect(screen.queryByText(/\(ref \d+\)/)).not.toBeInTheDocument();

    // No raw ids anywhere in the rendered rows.
    expect(screen.queryByText(/goose-yla6dp/)).not.toBeInTheDocument();
    expect(screen.queryByText(/maverick-4d9rp8/)).not.toBeInTheDocument();
    expect(screen.queryByText(/q1-heat/)).not.toBeInTheDocument();
  });

  it('clicking a pilot chip filters to that pilot; a heat chip filters to that heat', async () => {
    renderAudit();
    await waitFor(() => expect(rows()).toHaveLength(4));

    // Click Goose's chip on the DQ row → only the rows resolving to Goose remain (the DQ and
    // the reversal that chases to it), and the pilot select reflects the filter.
    await fireEvent.click(within(rows()[2]).getByRole('button', { name: 'Goose' }));
    await waitFor(() => expect(rows()).toHaveLength(2));
    expect((screen.getByLabelText('Filter by pilot') as HTMLSelectElement).value).toBe(
      'goose-yla6dp'
    );

    // Reset, then click the q2 heat chip → only q2's row remains.
    await fireEvent.click(screen.getByRole('button', { name: 'Clear filters' }));
    await waitFor(() => expect(rows()).toHaveLength(4));
    await fireEvent.click(within(rows()[1]).getByRole('button', { name: 'Qualifying R1 Heat 2' }));
    await waitFor(() => expect(rows()).toHaveLength(1));
    expect(rows()[0]).toHaveTextContent('Detection voided');
  });

  it('filters by kind and by free text over the rendered row', async () => {
    renderAudit();
    await waitFor(() => expect(rows()).toHaveLength(4));

    // The kind select offers exactly the badges present.
    const kind = screen.getByLabelText('Filter by kind') as HTMLSelectElement;
    const offered = Array.from(kind.options).map((o) => o.textContent?.trim());
    expect(offered).toEqual([
      'All kinds',
      'Ruling reversed',
      'Detection voided',
      'DQ',
      'Protest filed'
    ]);
    await fireEvent.change(kind, { target: { value: 'DQ' } });
    await waitFor(() => expect(rows()).toHaveLength(1));
    expect(rows()[0]).toHaveTextContent('DQ applied');

    await fireEvent.change(kind, { target: { value: '' } });
    // Free text matches the RENDERED text — the resolved callsign, not the raw ref.
    await fireEvent.input(screen.getByLabelText('Search audit entries'), {
      target: { value: 'maverick' }
    });
    await waitFor(() => expect(rows()).toHaveLength(1));
    expect(rows()[0]).toHaveTextContent('Protest filed: contact');
    // A raw ref must NOT match (nothing renders it).
    await fireEvent.input(screen.getByLabelText('Search audit entries'), {
      target: { value: 'maverick-4d9rp8' }
    });
    await waitFor(() =>
      expect(screen.getByText('No entries match the current filters.')).toBeInTheDocument()
    );
  });

  it('consumes (and clears) the cross-screen prefilter on mount', async () => {
    // Another screen deposited a heat prefilter and switched tabs (the auditFilter seam).
    const setTab = vi.fn();
    openAudit(setTab, { heat: 'q2-heat' });
    expect(setTab).toHaveBeenCalledWith('audit');

    renderAudit();
    // The heat filter arrives pre-set → only q2's entry shows.
    await waitFor(() => expect(rows()).toHaveLength(1));
    expect((screen.getByLabelText('Filter by heat') as HTMLSelectElement).value).toBe('q2-heat');
    // Consumed: nothing left in the mailbox for a later visit.
    expect(consumeAuditPrefilter()).toBeUndefined();
  });

  it('surfaces a visible error with retry when the audit read fails (#340 — no silent [])', async () => {
    let fail = true;
    renderAudit({
      eventAuditImpl: vi.fn(async () => {
        if (fail) throw new Error('boom');
        return ENTRIES;
      })
    });

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(/Couldn.t load the audit trail/);

    // Retry with the read healthy: the error clears and the trail renders.
    fail = false;
    await fireEvent.click(within(alert).getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    await waitFor(() => expect(rows()).toHaveLength(4));
  });

  it('resolves a FINISHED node-seeded heat’s rows from the durable heat-window binding', async () => {
    // A node-seeded heat: the entry's competitor is the raw `node-0` seat, the heat is FINISHED,
    // and the global live stream is on a DIFFERENT heat — so its progress can't resolve the seat
    // (the regression: the row rendered raw "node-0"). The durable `node-0 → Maverick` bind lives
    // in the heat's own `?projection=live` fold, pulled + cached via `session.ensureHeatBindings`.
    const NODE_ENTRY: EventAuditEntry[] = [
      {
        heat: 'q1-heat',
        kind: 'PenaltyApplied',
        at: 1_700_000_200_000_000,
        at_ref: 20,
        competitor: 'node-0',
        summary: 'DQ applied'
      }
    ];
    renderAudit({
      eventAuditImpl: vi.fn(async () => NODE_ENTRY),
      live: { current_heat: 'q2-heat', phase: 'Running' },
      heatFetches: {
        'q1-heat': {
          live: {
            current_heat: 'q1-heat',
            phase: 'Final',
            progress: [{ competitor: 'node-0', pilot: 'maverick-4d9rp8', laps_completed: 3 }]
          }
        }
      }
    });

    await waitFor(() => expect(rows()).toHaveLength(1));
    // The pilot chip resolves to the bound callsign — never the raw seat (CLAUDE.md).
    await waitFor(() =>
      expect(within(rows()[0]).getByRole('button', { name: 'Maverick' })).toBeInTheDocument()
    );
    expect(screen.queryByText(/node-0/)).not.toBeInTheDocument();
  });

  it('is a pure read — renders identically for a read-only session (no gated controls)', async () => {
    renderAudit({ role: 'readonly' });
    await waitFor(() => expect(rows()).toHaveLength(4));
    // The chips + filters are navigation/filtering, not mutations — all present read-only.
    expect(screen.getByLabelText('Filter by pilot')).toBeInTheDocument();
    expect(within(rows()[2]).getByRole('button', { name: 'Goose' })).toBeInTheDocument();
  });
});
