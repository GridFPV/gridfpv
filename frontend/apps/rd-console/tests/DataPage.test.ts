/**
 * The Data page: saving a copy of everything, and bringing one in from another computer.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { DataCopy, DataInfo } from '@gridfpv/types';
import DataPage from '../src/screens/DataPage.svelte';

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const COPY: DataCopy = { events: 2, made_at: 1_794_774_607_000_000, version: '0.4.0' };
const HERE: DataInfo = { folder: 'C:\\Grid\\gridfpv-data' };

function open(first: DataInfo, later: DataInfo = first) {
  const session = {
    getData: vi.fn<() => Promise<DataInfo>>().mockResolvedValueOnce(first).mockResolvedValue(later),
    exportData: vi.fn(async () => ({
      name: 'grid-data-2026-11-15-203007.zip',
      file: 'C:\\Grid\\gridfpv-data\\exports\\grid-data-2026-11-15-203007.zip',
      copy: COPY
    })),
    exportUrl: (name: string) => `/data/export/${name}`,
    importData: vi.fn(async (_file: Blob) => COPY as DataCopy | undefined),
    cancelImport: vi.fn(async () => true as const)
  };
  render(DataPage, { session });
  return session;
}

describe('DataPage', () => {
  it('says where the data is kept', async () => {
    open(HERE);
    expect(await screen.findByText('C:\\Grid\\gridfpv-data')).toBeVisible();
  });

  it('saves a copy, says where it is and offers it as a download', async () => {
    const session = open(HERE);
    await fireEvent.click(await screen.findByRole('button', { name: 'Save a copy' }));
    expect(await screen.findByText('Saved, with 2 events.')).toBeVisible();
    expect(session.exportData).toHaveBeenCalledOnce();
    expect(
      screen.getByText('C:\\Grid\\gridfpv-data\\exports\\grid-data-2026-11-15-203007.zip')
    ).toBeVisible();
    const link = screen.getByRole('link', { name: 'Download it here' });
    expect(link).toHaveAttribute('href', '/data/export/grid-data-2026-11-15-203007.zip');
  });

  it('asks before bringing a copy in, and sends nothing when the answer is no', async () => {
    const session = open(HERE);
    const file = new File(['zip'], 'grid-data-2026-11-01-120000.zip');
    await fireEvent.change(await screen.findByLabelText('Copy to bring in'), {
      target: { files: [file] }
    });
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText('grid-data-2026-11-01-120000.zip')).toBeVisible();
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(session.importData).not.toHaveBeenCalled();
  });

  it('brings a copy in and then says to restart Grid', async () => {
    const session = open(HERE, { ...HERE, waiting: COPY });
    const file = new File(['zip'], 'grid-data-2026-11-01-120000.zip');
    await fireEvent.change(await screen.findByLabelText('Copy to bring in'), {
      target: { files: [file] }
    });
    await fireEvent.click(await screen.findByRole('button', { name: 'Bring it in' }));
    await waitFor(() => expect(session.importData).toHaveBeenCalledWith(file));
    expect(
      await screen.findByText('A copy is waiting. Close Grid and open it again to finish.')
    ).toBeVisible();
  });

  it('shows why a file was refused', async () => {
    const session = open(HERE);
    session.importData.mockRejectedValueOnce(
      new Error('That file is not a copy made by Grid. It is not a zip file.')
    );
    await fireEvent.change(await screen.findByLabelText('Copy to bring in'), {
      target: { files: [new File(['x'], 'holiday.zip')] }
    });
    await fireEvent.click(await screen.findByRole('button', { name: 'Bring it in' }));
    expect(
      await screen.findByText('That file is not a copy made by Grid. It is not a zip file.')
    ).toBeVisible();
  });

  it('lets a waiting copy be called off', async () => {
    const session = open({ ...HERE, waiting: COPY }, HERE);
    await fireEvent.click(await screen.findByRole('button', { name: 'Do not bring it in' }));
    await waitFor(() => expect(session.cancelImport).toHaveBeenCalledOnce());
    await waitFor(() =>
      expect(
        screen.queryByText('A copy is waiting. Close Grid and open it again to finish.')
      ).toBeNull()
    );
  });

  it('says so when Grid keeps nothing on disk', async () => {
    open({});
    expect(await screen.findByText('Grid is not keeping its data in a folder.')).toBeVisible();
    expect(screen.queryByRole('button', { name: 'Save a copy' })).toBeNull();
  });
});
