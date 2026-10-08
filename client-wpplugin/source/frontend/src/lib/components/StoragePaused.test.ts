import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import StoragePaused from './StoragePaused.svelte';
import * as api from '../api/storage';
import { withEnglishLocale } from '../test-en-locale';

vi.mock('../api/storage', () => ({
  getStorageCapacity: vi.fn(),
  saveStorageCapacity: vi.fn(),
}));
vi.mock('../stores/toast', () => ({ showToast: vi.fn() }));

const snapshot = {
  policy: { format: 'wptsall-storage-v1', max_physical_bytes: 1 },
  revision: 'owned-cold-revision',
  physical_retained_bytes: 140,
  physical_retained_files: 1,
  root_booked_bytes: 0,
  max_physical_bytes: 1,
};

describe('components/StoragePaused', () => {
  beforeEach(() => {
    vi.mocked(api.getStorageCapacity).mockResolvedValue({ success: true, data: snapshot });
    vi.mocked(api.saveStorageCapacity).mockResolvedValue({ success: true, data: snapshot });
  });
  afterEach(() => { cleanup(); vi.clearAllMocks(); });

  it('explains retained, unavailable work in Chinese without empty counts', async () => {
    render(StoragePaused);
    await screen.findByTestId('storage-root-inventory');
    expect(screen.getByRole('alert').textContent).toContain('已有工作仍保留，并非空数据');
    expect(screen.getByRole('alert').textContent).toContain('重启客户端');
    expect(api.saveStorageCapacity).not.toHaveBeenCalled();
    expect(screen.queryByText('0 个任务')).toBeNull();
  });

  it('requires explicit capacity confirmation and retains restart guidance in English', async () => {
    await withEnglishLocale(async () => {
      render(StoragePaused);
      await screen.findByTestId('storage-root-inventory');
      expect(screen.getByRole('alert').textContent).toContain('Saved work is retained, not empty');
      await fireEvent.input(screen.getByLabelText('Data-root physical byte limit'), { target: { value: '33554432' } });
      expect(api.saveStorageCapacity).not.toHaveBeenCalled();
      await fireEvent.click(screen.getByRole('checkbox'));
      await fireEvent.click(screen.getByText('Save physical storage limit'));
      await waitFor(() => expect(api.saveStorageCapacity).toHaveBeenCalledWith(33554432, 'owned-cold-revision'));
      expect(screen.getByTestId('storage-paused-view')).toBeTruthy();
      expect(screen.getByRole('alert').textContent).toContain('restart the client');
    });
  });
});
