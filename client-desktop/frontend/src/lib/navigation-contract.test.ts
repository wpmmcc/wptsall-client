import { describe, expect, it } from 'vitest';

/**
 * Unified nav contract (WebUI + Desktop): same primary page ids.
 * Review is not a sidebar entry — entered from Tasks.
 */
export const UNIFIED_PRIMARY_PAGES = [
  'overview',
  'sites',
  'apikeys',
  'components',
  'tasks',
  'logs',
  'history',
  'settings',
] as const;

/** Webmaster self-serve journey order (站长自行配置). */
export const SELF_SERVE_JOURNEY = [
  'sites',
  'apikeys',
  'components',
  'tasks',
  'overview',
  'history',
] as const;

describe('desktop navigation contract (unified with WebUI)', () => {
  it('primary sidebar matches WebUI local-mode 8 entries', () => {
    expect(UNIFIED_PRIMARY_PAGES).toHaveLength(8);
    expect(UNIFIED_PRIMARY_PAGES).toContain('apikeys');
    expect(UNIFIED_PRIMARY_PAGES).not.toContain('credentials');
    expect(UNIFIED_PRIMARY_PAGES).not.toContain('providers');
    expect(UNIFIED_PRIMARY_PAGES).not.toContain('integration_pack');
  });

  it('self-serve journey is sites → keys → components → tasks → run → history', () => {
    expect(SELF_SERVE_JOURNEY[0]).toBe('sites');
    expect(SELF_SERVE_JOURNEY[1]).toBe('apikeys');
    expect(SELF_SERVE_JOURNEY[2]).toBe('components');
    expect(SELF_SERVE_JOURNEY[3]).toBe('tasks');
    expect(SELF_SERVE_JOURNEY[4]).toBe('overview');
  });
});
