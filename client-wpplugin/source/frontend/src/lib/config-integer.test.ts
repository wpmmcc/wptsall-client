// catalog: WEBUI-UI-Settings
// oracle: L2
import { describe, expect, it } from 'vitest';
import { configInteger } from './config-integer';

describe('worker config integer boundaries', () => {
  it.each([
    ['poll', 20, 1, 3600],
    ['domain concurrency', 3, 1, Number.MAX_SAFE_INTEGER],
    ['relation concurrency', 1, 1, Number.MAX_SAFE_INTEGER],
    ['global translation concurrency', 30, 1, Number.MAX_SAFE_INTEGER],
    ['global callback concurrency', 12, 1, Number.MAX_SAFE_INTEGER],
    ['relation callback backlog', 200, 1, Number.MAX_SAFE_INTEGER],
    ['callback concurrency', 4, 1, 50],
    ['callback timeout', 30, 1, 300],
    ['callback retries', 2, 0, 10],
    ['fetch timeout', 20, 1, 300],
    ['fetch retries', 2, 0, 10],
    ['adaptive delay', 5000, 200, Number.MAX_SAFE_INTEGER],
  ] as const)('%s preserves valid values and clamps boundaries', (_, fallback, min, max) => {
    expect(configInteger('0', fallback, min, max)).toBe(min);
    expect(configInteger('', fallback, min, max)).toBe(fallback);
    expect(configInteger('abc', fallback, min, max)).toBe(fallback);
    expect(configInteger(undefined, fallback, min, max)).toBe(fallback);
    expect(configInteger(Number.NaN, fallback, min, max)).toBe(fallback);
    expect(configInteger(Number.POSITIVE_INFINITY, fallback, min, max)).toBe(fallback);
    expect(configInteger('-1', fallback, min, max)).toBe(min);
    expect(configInteger(String(max + 1), fallback, min, max)).toBe(max);
    expect(configInteger(String(fallback), fallback, min, max)).toBe(fallback);
  });
});
