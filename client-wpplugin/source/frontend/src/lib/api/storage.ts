import { apiFetch } from './client';

export interface StorageSnapshot {
  policy: { format: string; max_physical_bytes: number };
  revision: string | null;
  physical_retained_bytes: number;
  physical_retained_files: number;
  root_booked_bytes: number;
  max_physical_bytes: number;
}

export const getStorageCapacity = () =>
  apiFetch<StorageSnapshot>('/api/storage/capacity');

export const saveStorageCapacity = (maxPhysicalBytes: number, revision: string | null) =>
  apiFetch<StorageSnapshot>('/api/storage/capacity', {
    method: 'POST',
    body: {
      max_physical_bytes: maxPhysicalBytes,
      expected_revision: revision,
      confirm_change: true,
    },
  });
