import { describe, expect, it } from 'vitest';

/** Catalog local-id helper mirrored from App.svelte for unit coverage. */
function catalogLocalId(item: {
  vendor_id?: string;
  template_id: string;
}): string {
  return `${item.vendor_id || 'provider'}-${item.template_id}`
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 120);
}

describe('catalogLocalId', () => {
  it('normalizes vendor/template into a safe local id', () => {
    expect(
      catalogLocalId({ vendor_id: 'OpenAI!', template_id: 'gpt-4o mini' })
    ).toBe('openai--gpt-4o-mini');
  });
});
