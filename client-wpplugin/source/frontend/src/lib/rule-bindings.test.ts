import { describe, expect, it } from 'vitest';
import {
  getRuleScopePlaceholder,
  normalizeRuleScopeKey,
  slotSupportsKind,
  validateRuleScopeKey,
} from './rule-bindings';

describe('rule-bindings helpers', () => {
  it('returns scope placeholders', () => {
    expect(getRuleScopePlaceholder('global')).toBe('global 作用域无需填写');
    expect(getRuleScopePlaceholder('plugin')).toBe('插件 slug，例如 woocommerce');
    expect(getRuleScopePlaceholder('relation')).toBe('站点关系 ID，例如 12');
    expect(getRuleScopePlaceholder('rule')).toBe('规则 ID，例如 128');
  });

  it('normalizes scope_key for submit payload', () => {
    expect(normalizeRuleScopeKey('global', ' 12 ')).toBeUndefined();
    expect(normalizeRuleScopeKey('plugin', '  woocommerce ')).toBe('woocommerce');
    expect(normalizeRuleScopeKey('plugin', '   ')).toBeUndefined();
  });

  it('validates required scope_key by scope', () => {
    expect(validateRuleScopeKey('global', '')).toBeNull();
    expect(validateRuleScopeKey('plugin', '')).toBe('当前作用域必须填写 scope_key');
    expect(validateRuleScopeKey('relation', 'abc')).toBe(
      'relation/rule 作用域的 scope_key 必须是正整数 ID'
    );
    expect(validateRuleScopeKey('rule', '12')).toBeNull();
  });

  it('matches slot and component kind compatibility', () => {
    expect(slotSupportsKind('plain_text', 'text')).toBe(true);
    expect(slotSupportsKind('plain_text', 'openai_compatible')).toBe(true);
    expect(slotSupportsKind('plain_text', 'image')).toBe(false);

    expect(slotSupportsKind('media_ref', 'image')).toBe(true);
    expect(slotSupportsKind('media_ref', 'video')).toBe(true);
    expect(slotSupportsKind('media_ref', 'text')).toBe(false);

    expect(slotSupportsKind('media_ref:image', 'image')).toBe(true);
    expect(slotSupportsKind('media_ref:image', 'video')).toBe(false);
    expect(slotSupportsKind('slug', 'mixed')).toBe(true);
  });
});
