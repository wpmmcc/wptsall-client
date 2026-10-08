import { beforeEach, expect, it, vi } from 'vitest';
import { getStorageCapacity, saveStorageCapacity } from './storage';
import { apiFetch } from './client';

vi.mock('./client', () => ({ apiFetch: vi.fn() }));
beforeEach(() => vi.clearAllMocks());

it('reads shared-root inventory without posting a policy change', () => {
  getStorageCapacity();
  expect(apiFetch).toHaveBeenCalledWith('/api/storage/capacity');
});

it('submits the exact loaded policy revision and explicit confirmation', () => {
  saveStorageCapacity(20000, 'owned-revision');
  expect(apiFetch).toHaveBeenCalledWith('/api/storage/capacity', {
    method: 'POST',
    body: { max_physical_bytes: 20000, expected_revision: 'owned-revision', confirm_change: true },
  });
});
