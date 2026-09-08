import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

/**
 * P0-LF-05: locale persistence must be purely browser-local.
 *
 * The i18n module initialises svelte-i18n at import time, so each test
 * re-imports it in a fresh module graph with a controlled localStorage seed
 * and navigator.language. fetch is stubbed to throw: any account-preference
 * round-trip would fail the "zero network calls" assertions loudly.
 */
const LS_KEY = 'wptsall_locale.v1';

async function loadI18n(
  opts: { stored?: string | null; navigatorLanguage?: string; preserveStorage?: boolean } = {},
) {
  vi.resetModules();
  if (!opts.preserveStorage) {
    localStorage.clear();
  }
  if (opts.stored != null) {
    localStorage.setItem(LS_KEY, opts.stored);
  }
  const navSpy = vi
    .spyOn(window.navigator, 'language', 'get')
    .mockReturnValue(opts.navigatorLanguage ?? 'en-US');
  const fetchMock = vi.fn(() => {
    throw new Error('network disabled by P0-LF-05 test');
  });
  vi.stubGlobal('fetch', fetchMock);

  const mod = await import('./index');
  const svelteI18n = await import('svelte-i18n');
  return { mod, svelteI18n, fetchMock, navSpy };
}

describe('i18n local-only locale persistence', () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    localStorage.clear();
  });

  it('stored en value initializes to en', async () => {
    const { mod, svelteI18n } = await loadI18n({ stored: 'en' });

    mod.initializeI18n();
    expect(get(svelteI18n.locale)).toBe('en');
    expect(document.documentElement.lang).toBe('en');
    expect(localStorage.getItem(LS_KEY)).toBe('en');
  });

  it('stored zh-CN value initializes to zh-CN', async () => {
    const { mod, svelteI18n } = await loadI18n({ stored: 'zh-CN' });

    mod.initializeI18n();
    expect(get(svelteI18n.locale)).toBe('zh-CN');
    expect(document.documentElement.lang).toBe('zh-CN');
    expect(localStorage.getItem(LS_KEY)).toBe('zh-CN');
  });

  it('unsupported and corrupt stored values fall back deterministically', async () => {
    // Corrupt value falls back to the navigator tag (zh-TW → zh-CN), never throws.
    const corrupt = await loadI18n({ stored: '{"json":', navigatorLanguage: 'zh-TW' });
    corrupt.mod.initializeI18n();
    expect(get(corrupt.svelteI18n.locale)).toBe('zh-CN');
    expect(localStorage.getItem(LS_KEY)).toBe('zh-CN');

    // Unsupported locale with an English navigator falls back to en.
    const unsupported = await loadI18n({ stored: 'fr-FR', navigatorLanguage: 'en-GB' });
    unsupported.mod.initializeI18n();
    expect(get(unsupported.svelteI18n.locale)).toBe('en');
    expect(localStorage.getItem(LS_KEY)).toBe('en');
  });

  it('normalizes navigator.language BCP 47 tags when nothing is stored', async () => {
    const zhVariant = await loadI18n({ navigatorLanguage: 'zh-TW' });
    expect(get(zhVariant.svelteI18n.locale)).toBe('zh-CN');

    const enVariant = await loadI18n({ navigatorLanguage: 'en-GB' });
    expect(get(enVariant.svelteI18n.locale)).toBe('en');

    // Unrelated language defaults to English.
    const other = await loadI18n({ navigatorLanguage: 'fr-FR' });
    expect(get(other.svelteI18n.locale)).toBe('en');
  });

  it('changing locale writes the versioned localStorage key', async () => {
    const { mod, svelteI18n } = await loadI18n({ stored: 'en' });

    mod.initializeI18n();
    mod.setLocale('zh-CN');
    expect(get(svelteI18n.locale)).toBe('zh-CN');
    expect(document.documentElement.lang).toBe('zh-CN');
    expect(localStorage.getItem(LS_KEY)).toBe('zh-CN');

    // Round-trip: a fresh graph reads the value back.
    const reloaded = await loadI18n({ navigatorLanguage: 'en-US', preserveStorage: true });
    expect(get(reloaded.svelteI18n.locale)).toBe('zh-CN');
  });

  it('initialization and update make zero fetch calls', async () => {
    const { mod, fetchMock } = await loadI18n({ stored: 'zh-CN', navigatorLanguage: 'zh-CN' });

    mod.initializeI18n();
    mod.setLocale('en');
    mod.setLocale('zh-CN');

    expect(fetchMock).not.toHaveBeenCalled();
  });
});
