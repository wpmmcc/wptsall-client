import { describe, expect, it } from 'vitest';

const ACCEPTANCE_STEPS = [
  'import site connection pack',
  'save vendor key',
  'quick-test component against local mock-api',
  'run worker once',
  'review job item',
] as const;

describe('P1-G-2 acceptance journey labels', () => {
  it('lists five steps in order', () => {
    expect(ACCEPTANCE_STEPS).toHaveLength(5);
    expect(ACCEPTANCE_STEPS[0]).toMatch(/import/i);
    expect(ACCEPTANCE_STEPS[2]).toMatch(/mock/i);
    expect(ACCEPTANCE_STEPS[4]).toMatch(/review/i);
  });

  it('does not require website OAuth login', () => {
    expect(ACCEPTANCE_STEPS.join(' ')).not.toMatch(/oauth login|官网|wpmm\.cc/i);
  });
});
