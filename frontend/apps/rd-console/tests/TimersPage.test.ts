/**
 * The Timers page against a mocked session: the list with each timer's status in words, the
 * add dialog (with the Director's refusal shown as it is), the two-step remove, and the
 * 2 second poll that stops when the page closes.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { DiscoveredTimer, Timer, TimerView } from '@gridfpv/types';
import TimersPage from '../src/screens/TimersPage.svelte';
import { timerView } from './competition.js';
import { makeTestSession } from './support.js';

const GATE = timerView('gate-x1y2', 'Gate', 4, { clock_error_micros: 200 });
const SAID = 'Nothing answered at 127.0.0.1:9.';
const OFF: TimerView = {
  timer: { id: 'far-gate-9zz0', name: 'Far Gate', kind: 'Oft', address: '127.0.0.1:9' },
  status: { connected: false, problem: SAID, seats: [] }
};

afterEach(() => vi.useRealTimers());

describe('TimersPage', () => {
  it('says no timer is added yet, and that the bench seats still work', async () => {
    const { session } = makeTestSession({ noEnter: true });
    render(TimersPage, { session, onhome: vi.fn() });
    expect(
      await screen.findByText('No timer is added yet. Heats can still be run on the bench seats.')
    ).toBeVisible();
  });

  it('lists each timer with its name, address, status, seats and clock', async () => {
    const { session } = makeTestSession({
      noEnter: true,
      listTimersImpl: vi.fn(async () => [GATE, OFF])
    });
    render(TimersPage, { session, onhome: vi.fn() });

    const gate = await screen.findByRole('listitem', { name: 'Gate' });
    expect(gate).toHaveTextContent('192.168.1.50:7270');
    expect(within(gate).getByText('Connected')).toBeVisible();
    expect(gate).toHaveTextContent('4 seats');
    expect(gate).toHaveTextContent('Clock known to within 0.2 ms');
    expect(within(gate).queryByTestId('timer-problem')).toBeNull();

    const off = screen.getByRole('listitem', { name: 'Far Gate' });
    expect(off).toHaveTextContent('127.0.0.1:9');
    expect(within(off).getByText('Not connected')).toBeVisible();
    // The problem is the Director's sentence, as it wrote it.
    expect(within(off).getByTestId('timer-problem')).toHaveTextContent(SAID);
    expect(off).toHaveTextContent('No seats reported yet');
    expect(off).toHaveTextContent('Clock not measured yet');

    // No raw timer id on the page.
    for (const id of [GATE.timer.id, OFF.timer.id]) {
      expect(document.body.textContent).not.toContain(id);
    }
  });

  it('adds a timer from the dialog and reads the list again', async () => {
    const created: Timer = { id: 'gate-x1y2', name: 'Gate', kind: 'Oft', address: '10.0.0.5:7270' };
    const list: TimerView[] = [];
    const createTimerImpl = vi.fn(async () => {
      list.push({ timer: created, status: { connected: false, seats: [] } });
      return created;
    });
    const { session } = makeTestSession({
      noEnter: true,
      listTimersImpl: vi.fn(async () => [...list]),
      createTimerImpl
    });
    render(TimersPage, { session, onhome: vi.fn() });

    await fireEvent.click(screen.getByRole('button', { name: '+ Add timer' }));
    const form = await screen.findByRole('form', { name: 'Add timer' });
    expect(form).toHaveTextContent('The port is 7270 unless it was changed.');
    expect(within(form).getByLabelText('Address')).toHaveAttribute(
      'placeholder',
      '192.168.1.50:7270'
    );
    await fireEvent.input(within(form).getByLabelText('Name'), { target: { value: ' Gate ' } });
    await fireEvent.input(within(form).getByLabelText('Address'), {
      target: { value: '10.0.0.5:7270' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add timer' }));

    await waitFor(() =>
      expect(createTimerImpl).toHaveBeenCalledWith(
        'http://d.local',
        { name: 'Gate', kind: 'Oft', address: '10.0.0.5:7270' },
        'tok'
      )
    );
    expect(await screen.findByRole('listitem', { name: 'Gate' })).toBeVisible();
    await waitFor(() => expect(screen.queryByRole('form', { name: 'Add timer' })).toBeNull());
  });

  it("shows the Director's sentence in the dialog when it says no, and stays open", async () => {
    const REFUSED = 'A timer needs an address, like 192.168.1.50:7270.';
    const { session } = makeTestSession({
      noEnter: true,
      createTimerImpl: vi.fn(async () => {
        throw Object.assign(new Error(REFUSED), { status: 400 });
      })
    });
    render(TimersPage, { session, onhome: vi.fn() });
    await fireEvent.click(screen.getByRole('button', { name: '+ Add timer' }));
    const form = await screen.findByRole('form', { name: 'Add timer' });
    await fireEvent.input(within(form).getByLabelText('Name'), { target: { value: 'Gate' } });
    await fireEvent.input(within(form).getByLabelText('Address'), { target: { value: 'nope' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add timer' }));
    expect(await within(form).findByText(REFUSED)).toBeVisible();
    expect(screen.getByRole('form', { name: 'Add timer' })).toBeVisible();
  });

  it('removes a timer only after the second click', async () => {
    const list = [GATE];
    const deleteTimerImpl = vi.fn(async () => {
      list.length = 0;
    });
    const { session } = makeTestSession({
      noEnter: true,
      listTimersImpl: vi.fn(async () => [...list]),
      deleteTimerImpl
    });
    render(TimersPage, { session, onhome: vi.fn() });
    const gate = await screen.findByRole('listitem', { name: 'Gate' });
    await fireEvent.click(within(gate).getByRole('button', { name: 'Remove' }));
    expect(deleteTimerImpl).not.toHaveBeenCalled();
    await fireEvent.click(within(gate).getByRole('button', { name: 'Confirm' }));
    await waitFor(() =>
      expect(deleteTimerImpl).toHaveBeenCalledWith('http://d.local', 'gate-x1y2', 'tok')
    );
    await waitFor(() => expect(screen.queryByRole('listitem', { name: 'Gate' })).toBeNull());
  });

  it("opens a timer's Tune page from its row, by the timer's id", async () => {
    const { session } = makeTestSession({
      noEnter: true,
      listTimersImpl: vi.fn(async () => [GATE, OFF])
    });
    const ontune = vi.fn();
    render(TimersPage, { session, onhome: vi.fn(), ontune });
    const off = await screen.findByRole('listitem', { name: 'Far Gate' });
    await fireEvent.click(within(off).getByRole('button', { name: 'Tune' }));
    expect(ontune).toHaveBeenCalledWith('far-gate-9zz0');
  });

  it('reads both lists every 2 seconds while open, and stops when it closes', async () => {
    vi.useFakeTimers();
    const listTimersImpl = vi.fn(async (): Promise<TimerView[]> => []);
    const listDiscoveredTimersImpl = vi.fn(async (): Promise<DiscoveredTimer[]> => []);
    const { session } = makeTestSession({
      noEnter: true,
      listTimersImpl,
      listDiscoveredTimersImpl
    });
    const { unmount } = render(TimersPage, { session, onhome: vi.fn() });
    await vi.advanceTimersByTimeAsync(0);
    expect(listTimersImpl).toHaveBeenCalledTimes(1);
    expect(listDiscoveredTimersImpl).toHaveBeenCalledTimes(1);
    expect(listDiscoveredTimersImpl).toHaveBeenCalledWith('http://d.local', { token: 'tok' });
    await vi.advanceTimersByTimeAsync(4000);
    expect(listTimersImpl).toHaveBeenCalledTimes(3);
    expect(listDiscoveredTimersImpl).toHaveBeenCalledTimes(3);
    unmount();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(listTimersImpl).toHaveBeenCalledTimes(3);
    expect(listDiscoveredTimersImpl).toHaveBeenCalledTimes(3);
  });

  describe('found on the network', () => {
    const NONE =
      'Timers that announce themselves on the network show up here, and a timer can always be added by its address.';
    const FAR: DiscoveredTimer = {
      name: 'Far Gate',
      address: '10.0.0.9:7270',
      seats: 4,
      added: false
    };
    const BENCH: DiscoveredTimer = { name: 'Bench Gate', address: '10.0.0.7:7270', added: false };
    const KNOWN: DiscoveredTimer = {
      name: 'oft-gate',
      address: '192.168.1.50:7270',
      seats: 1,
      added: true
    };
    const foundList = () => screen.getByRole('list', { name: 'Found on the network' });

    it('says where found timers show up when there is none', async () => {
      const { session } = makeTestSession({ noEnter: true });
      render(TimersPage, { session, onhome: vi.fn() });
      expect(screen.getByRole('heading', { name: 'Found on the network' })).toBeVisible();
      expect(await screen.findByText(NONE)).toBeVisible();
      expect(screen.queryByRole('list', { name: 'Found on the network' })).toBeNull();
    });

    it('lists a found timer with its name, address and seats, and not one already added', async () => {
      const { session } = makeTestSession({
        noEnter: true,
        listTimersImpl: vi.fn(async () => [GATE]),
        listDiscoveredTimersImpl: vi.fn(async () => [
          KNOWN,
          FAR,
          BENCH,
          { ...FAR, name: 'Solo', address: '10.0.0.3:7270', seats: 1 }
        ])
      });
      render(TimersPage, { session, onhome: vi.fn() });
      const far = await waitFor(() =>
        within(foundList()).getByRole('listitem', { name: 'Far Gate' })
      );
      expect(far).toHaveTextContent('10.0.0.9:7270');
      expect(far).toHaveTextContent('4 seats');
      expect(within(far).getByRole('button', { name: 'Add' })).toBeEnabled();
      // The seat count is left out when the timer did not say.
      const bench = within(foundList()).getByRole('listitem', { name: 'Bench Gate' });
      expect(bench).toHaveTextContent('10.0.0.7:7270');
      expect(bench).not.toHaveTextContent(/seat/);
      expect(within(foundList()).getByRole('listitem', { name: 'Solo' })).toHaveTextContent(
        '1 seat'
      );
      // The one already added is in the timer list, not here.
      expect(within(foundList()).getAllByRole('listitem')).toHaveLength(3);
      expect(within(foundList()).queryByText('oft-gate')).toBeNull();
      expect(screen.queryByText(NONE)).toBeNull();
    });

    it('says the same sentence when every found timer is already added', async () => {
      const { session } = makeTestSession({
        noEnter: true,
        listTimersImpl: vi.fn(async () => [GATE]),
        listDiscoveredTimersImpl: vi.fn(async () => [KNOWN])
      });
      render(TimersPage, { session, onhome: vi.fn() });
      expect(await screen.findByText(NONE)).toBeVisible();
      expect(screen.queryByText('oft-gate')).toBeNull();
    });

    it('adds a found timer in one click, with its name and address, and reads both lists again', async () => {
      const created: Timer = {
        id: 'far-gate-9zz0',
        name: 'Far Gate',
        kind: 'Oft',
        address: FAR.address
      };
      const list: TimerView[] = [];
      let found: DiscoveredTimer[] = [FAR];
      const createTimerImpl = vi.fn(async () => {
        list.push({ timer: created, status: { connected: false, seats: [] } });
        found = [{ ...FAR, added: true }];
        return created;
      });
      const { session } = makeTestSession({
        noEnter: true,
        listTimersImpl: vi.fn(async () => [...list]),
        listDiscoveredTimersImpl: vi.fn(async () => found),
        createTimerImpl
      });
      render(TimersPage, { session, onhome: vi.fn() });
      const far = await waitFor(() =>
        within(foundList()).getByRole('listitem', { name: 'Far Gate' })
      );
      await fireEvent.click(within(far).getByRole('button', { name: 'Add' }));
      await waitFor(() =>
        expect(createTimerImpl).toHaveBeenCalledWith(
          'http://d.local',
          { name: 'Far Gate', kind: 'Oft', address: '10.0.0.9:7270' },
          'tok'
        )
      );
      // It moves to the timer list, and nothing is left to add.
      await waitFor(() =>
        expect(
          within(screen.getByRole('list', { name: 'Timers' })).getByRole('listitem', {
            name: 'Far Gate'
          })
        ).toBeVisible()
      );
      expect(await screen.findByText(NONE)).toBeVisible();
      // No dialog was opened for it, and the id is not on the page.
      expect(screen.queryByRole('form', { name: 'Add timer' })).toBeNull();
      expect(document.body.textContent).not.toContain(created.id);
    });

    it("shows the Director's sentence when the add is refused, and keeps the timer listed", async () => {
      const REFUSED = 'A timer needs a name.';
      const { session } = makeTestSession({
        noEnter: true,
        listDiscoveredTimersImpl: vi.fn(async () => [FAR]),
        createTimerImpl: vi.fn(async () => {
          throw Object.assign(new Error(REFUSED), { status: 400 });
        })
      });
      render(TimersPage, { session, onhome: vi.fn() });
      const far = await waitFor(() =>
        within(foundList()).getByRole('listitem', { name: 'Far Gate' })
      );
      await fireEvent.click(within(far).getByRole('button', { name: 'Add' }));
      const problem = await screen.findByTestId('found-problem');
      expect(problem).toHaveTextContent('The timer was not added.');
      expect(problem).toHaveTextContent(REFUSED);
      expect(within(foundList()).getByRole('listitem', { name: 'Far Gate' })).toBeVisible();
      await waitFor(() => expect(within(far).getByRole('button', { name: 'Add' })).toBeEnabled());
    });

    it("says so, in the Director's words, when the network could not be searched", async () => {
      const SAID_NO = 'The Director cannot look for timers right now.';
      const { session } = makeTestSession({
        noEnter: true,
        listTimersImpl: vi.fn(async () => [GATE]),
        listDiscoveredTimersImpl: vi.fn(async () => {
          throw new Error(SAID_NO);
        })
      });
      render(TimersPage, { session, onhome: vi.fn() });
      expect(await screen.findByText(SAID_NO)).toBeVisible();
      // The timer list is still shown.
      expect(screen.getByRole('listitem', { name: 'Gate' })).toBeVisible();
    });
  });
});
