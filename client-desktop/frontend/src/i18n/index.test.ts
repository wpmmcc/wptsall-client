import { describe, expect, it } from 'vitest';
import { get } from 'svelte/store';
import { locale } from 'svelte-i18n';
import en from './locales/en.json';
import zhCN from './locales/zh-CN.json';
import { availableLocales, setLocale, t } from './index';

type Dict = Record<string, unknown>;

function keyPaths(obj: Dict, prefix = ''): string[] {
  return Object.entries(obj).flatMap(([k, v]) =>
    typeof v === 'object' && v !== null
      ? keyPaths(v as Dict, `${prefix}${k}.`)
      : [`${prefix}${k}`]
  );
}

const enPaths = new Set(keyPaths(en as Dict));
const zhPaths = new Set(keyPaths(zhCN as Dict));

describe('desktop i18n dictionaries', () => {
  it('en and zh-CN expose identical key sets', () => {
    expect(zhPaths).toEqual(enPaths);
  });

  it('covers every navigation page key rendered by App.svelte', () => {
    const pages = [
      'overview',
      'sites',
      'credentials',
      'tasks',
      'components',
      'providers',
      'integration_pack',
      'history',
      'logs',
      'settings',
    ];
    for (const page of pages) {
      // a resolved message never equals the raw key
      expect(t(`nav.${page}`), `nav.${page}`).not.toBe(`nav.${page}`);
    }
  });

  it('advertises exactly en and zh-CN', () => {
    expect([...availableLocales]).toEqual(['en', 'zh-CN']);
  });
});

describe('desktop i18n runtime', () => {
  it('t() translates in both locales and interpolates params', () => {
    setLocale('en');
    expect(t('nav.sites')).toBe('Sites');
    expect(t('msg.batch_deleted', { count: 3 })).toBe('Deleted 3 translation record(s)');
    setLocale('zh-CN');
    expect(t('nav.sites')).toBe('站点');
    expect(t('msg.batch_deleted', { count: 3 })).toBe('已删除 3 条翻译记录');
  });

  it('setLocale normalizes loose input and rejects unsupported codes', () => {
    setLocale('zh_HANS');
    expect(get(locale)).toBe('zh-CN');
    setLocale('fr');
    expect(get(locale)).toBe('en');
  });

  it('t() falls back to the raw key when a message is missing', () => {
    setLocale('en');
    expect(t('no.such.key')).toBe('no.such.key');
  });
});
