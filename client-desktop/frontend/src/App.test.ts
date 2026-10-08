import { expect, it } from 'vitest';
import appSource from './App.svelte?raw';

it('shows the shared capacity pause before ordinary pages when the database is unavailable', () => {
  expect(appSource).toMatch(/\{#if \$status\?\.storage_paused && \$status\.database_available === false\}/);
  expect(appSource).toMatch(/import StoragePaused from '@webui\/lib\/components\/StoragePaused\.svelte'/);
  expect(appSource).toMatch(/<StoragePaused\s*\/>/);
  expect(appSource.indexOf('<StoragePaused')).toBeLessThan(appSource.indexOf('<Overview'));
});
