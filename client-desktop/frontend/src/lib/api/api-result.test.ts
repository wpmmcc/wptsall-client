import { describe, expect, it } from 'vitest';
import type { ApiResult } from './types';
import { isOk } from './client';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { vi } from 'vitest';

describe('ApiResult helpers', () => {
  it('isOk narrows success payloads', () => {
    const ok: ApiResult<{ id: string }> = { success: true, data: { id: 'x' } };
    const err: ApiResult<{ id: string }> = {
      success: false,
      error: { code: 'X', message: 'nope' },
    };
    expect(isOk(ok)).toBe(true);
    if (isOk(ok)) expect(ok.data.id).toBe('x');
    expect(isOk(err)).toBe(false);
  });
});
