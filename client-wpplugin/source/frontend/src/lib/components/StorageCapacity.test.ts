import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import StorageCapacity from './StorageCapacity.svelte';
import * as api from '../api/storage';
import { withEnglishLocale } from '../test-en-locale';

vi.mock('../api/storage', () => ({
  getStorageCapacity: vi.fn(),
  saveStorageCapacity: vi.fn(),
}));
vi.mock('../stores/toast', () => ({ showToast: vi.fn() }));

const snapshot = {
  policy: { format: 'wptsall-storage-v1', max_physical_bytes: 32000 },
  revision: 'owned-revision',
  physical_retained_bytes: 1350,
  physical_retained_files: 3,
  root_booked_bytes: 4096,
  max_physical_bytes: 32000,
};
const loaded = () => ({ success: true as const, data: structuredClone(snapshot) });

describe('components/StorageCapacity', () => {
  beforeEach(() => {
    vi.mocked(api.getStorageCapacity).mockResolvedValue(loaded());
    vi.mocked(api.saveStorageCapacity).mockResolvedValue(loaded());
  });
  afterEach(() => { cleanup(); vi.clearAllMocks(); });

  it('shows physical copies and separate paid bookings in Chinese', async () => {
    render(StorageCapacity);
    expect((await screen.findByTestId('storage-root-inventory')).textContent).toContain('1350');
    expect(screen.getByTestId('storage-root-inventory').textContent).toContain('4096');
    expect(screen.getByText(/不是所有写入或原生平台的完整保证/)).toBeTruthy();
    expect((screen.getByText('保存实际容量上限') as HTMLButtonElement).disabled).toBe(true);
  });

  it('saves only after explicit confirmation with the loaded revision', async () => {
    await withEnglishLocale(async () => {
      render(StorageCapacity);
      await screen.findByTestId('storage-root-inventory');
      const input = screen.getByLabelText('Data-root physical byte limit') as HTMLInputElement;
      await fireEvent.input(input, { target: { value: '20000' } });
      expect(api.saveStorageCapacity).not.toHaveBeenCalled();
      await fireEvent.click(screen.getByRole('checkbox'));
      await fireEvent.click(screen.getByText('Save physical storage limit'));
      await waitFor(() => expect(api.saveStorageCapacity).toHaveBeenCalledWith(20000, 'owned-revision'));
      await waitFor(() => expect((screen.getByRole('checkbox') as HTMLInputElement).checked).toBe(false));
    });
  });

  it('retains a failed draft and blocks retry until explicit reload', async () => {
    await withEnglishLocale(async () => {
      vi.mocked(api.saveStorageCapacity).mockResolvedValue({
        success: false, error: { code: 'STORAGE_CAPACITY_CHANGED', message: 'owned stale policy' },
      });
      render(StorageCapacity);
      await screen.findByTestId('storage-root-inventory');
      const input = screen.getByLabelText('Data-root physical byte limit') as HTMLInputElement;
      await fireEvent.input(input, { target: { value: '20000' } });
      await fireEvent.click(screen.getByRole('checkbox'));
      await fireEvent.click(screen.getByText('Save physical storage limit'));
      await screen.findByRole('alert');
      expect(input.value).toBe('20000');
      expect((screen.getByText('Save physical storage limit') as HTMLButtonElement).disabled).toBe(true);
      await fireEvent.click(screen.getByText('Reload physical inventory'));
      await waitFor(() => expect(input.value).toBe('32000'));
      expect((screen.getByRole('checkbox') as HTMLInputElement).checked).toBe(false);
    });
  });

  it('does not turn unreadable inventory into zero usage or permission', async () => {
    await withEnglishLocale(async () => {
      vi.mocked(api.getStorageCapacity).mockResolvedValue({
        success: false, error: { code: 'STORAGE_CAPACITY_INVALID', message: 'owned damaged control' },
      });
      render(StorageCapacity);
      await screen.findByRole('alert');
      expect(screen.queryByTestId('storage-root-inventory')).toBeNull();
      expect((screen.getByText('Save physical storage limit') as HTMLButtonElement).disabled).toBe(true);
      expect(api.saveStorageCapacity).not.toHaveBeenCalled();
    });
  });

  it.each(['0', '-1', '1.5', '9007199254740992'])('refuses invalid limit %s without a write', async value => {
    await withEnglishLocale(async () => {
      render(StorageCapacity);
      await screen.findByTestId('storage-root-inventory');
      await fireEvent.input(screen.getByLabelText('Data-root physical byte limit'), { target: { value } });
      await fireEvent.click(screen.getByRole('checkbox'));
      await fireEvent.click(screen.getByText('Save physical storage limit'));
      expect(api.saveStorageCapacity).not.toHaveBeenCalled();
    });
  });
});
