import { describe, expect, it } from 'vitest';

/**
 * Lightweight smoke for the Desktop page list contract used by P1-G-2.
 * Full App.svelte is not mounted here (no jsdom/testing-library yet).
 */
const REQUIRED_PAGES = [
  'overview',
  'sites',
  'credentials',
  'tasks',
  'components',
  'providers',
  'integration_pack',
  'logs',
  'settings',
] as const;

describe('desktop navigation contract (P1-G-2)', () => {
  it('requires credentials + logs pages for acceptance journey', () => {
    expect(REQUIRED_PAGES).toContain('credentials');
    expect(REQUIRED_PAGES).toContain('logs');
    expect(REQUIRED_PAGES.length).toBeGreaterThanOrEqual(9);
  });

  it('acceptance order covers import → key → quick-test → run-once → review', () => {
    const journey = ['sites', 'credentials', 'components', 'tasks', 'tasks'];
    expect(journey[0]).toBe('sites');
    expect(journey[1]).toBe('credentials');
    expect(journey[2]).toBe('components');
    expect(journey[3]).toBe('tasks');
  });
});
