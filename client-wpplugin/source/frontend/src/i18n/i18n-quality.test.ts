// catalog: WEBUI-I18N-QUALITY
// oracle: L2
// 状态矩阵：EN 词典无 key 回显占位值 / EN 词典无中文残留 / zh 词典无孤儿键（en↔zh 键一致）。
// 背景（tasks/client2/27 §UI-27-04/05）：测试环境曾强制 zh-CN + 仅装载 shared 词典，
// EN 渲染在 CI 结构性不可见，导致 26 处未填占位值与 88 个 zh 孤儿键长期绿灯。
// 本用例使词典质量成为永久门禁（对运行时同一合并词典断言）。
import { describe, expect, it } from 'vitest';
import { buildLocaleMessages, unflatten } from './messages';
import sharedEn from '../../../locales/en.json';
import sharedZhCN from '../../../locales/zh-CN.json';
import pageEnDict from './locales/en.json';
import pageZhDict from './locales/zh-CN.json';

function flatten(dict: Record<string, unknown>, path = ''): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(dict)) {
    const p = path ? `${path}.${key}` : key;
    if (value && typeof value === 'object' && !Array.isArray(value)) {
      Object.assign(out, flatten(value as Record<string, unknown>, p));
    } else {
      out[p] = String(value);
    }
  }
  return out;
}

const messages = buildLocaleMessages();
const en = flatten(messages.en as Record<string, unknown>);
const zh = flatten(messages['zh-CN'] as Record<string, unknown>);

/**
 * Legitimate labels whose value coincidentally equals the Title-Cased key
 * suffix. Keep this list EXPLICIT: a key may join it only with a comment
 * explaining why the echo is the intended UI text.
 */
const LEGIT_ECHO_KEYS = new Set([
  // The panel title for per-domain drill-down details — "Domain Detail" is
  // the intended English label.
  'overview.domain_detail',
  // Field label for the slot key input — "Slot Key" is the intended label.
  'task_routing.slot_key_label',
]);

/** Meta-word suffixes that mark a value as an unfilled key-echo placeholder. */
const META_SUFFIX = /(desc|footer|detail|placeholder|label|title)$/;

function isKeyEcho(key: string, value: string): boolean {
  const suffix = key.split('.').pop() ?? '';
  const echo = suffix.replace(/_/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase());
  return value === echo && META_SUFFIX.test(suffix) && value.includes(' ');
}

describe('i18n dictionary quality (runtime-merged dicts)', () => {
  it('EN dictionary has no unfilled key-echo placeholder values', () => {
    const offenders = Object.entries(en)
      .filter(([key, value]) => isKeyEcho(key, value) && !LEGIT_ECHO_KEYS.has(key))
      .map(([key, value]) => `${key} = "${value}"`);
    expect(offenders, `unfilled EN placeholders (write real English or delete the page-dict override so the shared value wins):\n${offenders.join('\n')}`).toEqual([]);
  });

  it('EN dictionary renders no Chinese (except the intentional language-switcher label)', () => {
    const cjk = /[\u4e00-\u9fff]/;
    const offenders = Object.entries(en)
      .filter(([key, value]) => key !== 'language.zh_CN' && cjk.test(value))
      .map(([key, value]) => `${key} = "${value}"`);
    expect(offenders, `EN values containing Chinese (mixed-locale leak):\n${offenders.join('\n')}`).toEqual([]);
  });

  it('zh-CN dictionary has no orphan keys missing from EN (locale parity)', () => {
    const orphans = Object.keys(zh).filter((key) => !(key in en));
    expect(orphans, `zh-CN keys with no EN counterpart (dead keys — delete them, or add the EN value):\n${orphans.join('\n')}`).toEqual([]);
  });

  it('EN dictionary has no orphan keys missing from zh-CN (locale parity, reverse direction)', () => {
    // 批 M / Y-15 收尾: the reverse leg was untested, so 33 EN keys (24
    // sites.* + templates/worker/key_modal) shipped with no zh value and
    // zh users fell back on them. Bidirectional parity is now a permanent
    // gate: adding a key to ONE dictionary without the other goes red.
    const orphans = Object.keys(en).filter((key) => !(key in zh));
    expect(orphans, `EN keys with no zh-CN counterpart (add the zh value, or delete the key):\n${orphans.join('\n')}`).toEqual([]);
  });

  it('shared flat dictionaries have no duplicate/conflicting dotted keys', () => {
    // Structural sanity: every shared flat key must survive the merge into
    // the runtime dict (page dicts override per-key, never wipe sections).
    const missing = Object.keys(sharedEn).filter((key) => !(key in en));
    expect(missing, `shared EN keys lost by the merge:\n${missing.join('\n')}`).toEqual([]);
    const missingZh = Object.keys(sharedZhCN).filter((key) => !(key in zh));
    expect(missingZh, `shared zh-CN keys lost by the merge:\n${missingZh.join('\n')}`).toEqual([]);
  });

  it('flat dictionaries are exactly the 24-key Rust domain (no UI keys)', () => {
    // 批 P 域门：flat 册（client-wpplugin/source/locales/*.json）是 Rust
    // 独占层——CLI/OAuth eprintln 与 tray 菜单的 24 个 `i18n::t("...")`
    // 字面量。UI 键漂回此层 = 基层/覆写双份维护（zh 477 影子键值漂移的
    // 事故根源）；此处少于 24 = CLI 裸键疤（t() fallback 恒回显键名）。
    // 与 client src/i18n.rs 的 Rust 侧域门互为镜像。
    const rustDomain = new Set([
      'cli.domains_fetch_failed', 'cli.heartbeat_failed', 'cli.invalid_config',
      'cli.login_failed', 'cli.missing_wp_token', 'cli.oauth_manual_url',
      'cli.oauth_opening_browser', 'cli.oauth_success', 'cli.oauth_timeout',
      'cli.oauth_waiting_callback', 'cli.process_domain_failed',
      'cli.process_domain_warning', 'cli.session_expired_relogin',
      'cli.webui_listening', 'oauth_callback.close_window',
      'oauth_callback.login_failed', 'oauth_callback.login_successful',
      'oauth_callback.no_auth_code', 'oauth_callback.no_session',
      'oauth_callback.state_mismatch', 'oauth_callback.token_exchange_error_prefix',
      'tray.show', 'tray.run_once', 'tray.quit',
    ]);
    const offenders = (dict: Record<string, string>) =>
      Object.keys(dict).filter((key) => !rustDomain.has(key));
    expect(
      offenders(sharedEn as Record<string, string>),
      'flat EN keys outside the Rust domain (move them to the page dictionary)',
    ).toEqual([]);
    expect(
      offenders(sharedZhCN as Record<string, string>),
      'flat zh-CN keys outside the Rust domain (move them to the page dictionary)',
    ).toEqual([]);
    expect(
      Object.keys(sharedEn).length,
      'flat EN must carry exactly the 24 Rust literals',
    ).toBe(24);
    expect(
      Object.keys(sharedZhCN).length,
      'flat zh-CN must carry exactly the 24 Rust literals',
    ).toBe(24);
  });

  it('flat and page layers stay disjoint (per language)', () => {
    // 批 P 层门：两册系有意的基层/覆写双层架构（flat=Rust 域独占，
    // page=WebUI SPA 域）。层相交即覆写恒赢、基层值死——zh 477 影子
    // 键值漂移（覆写层未同步基层修订）即此类事故。
    const pageEn = flatten(pageEnDict as Record<string, unknown>);
    const pageZh = flatten(pageZhDict as Record<string, unknown>);
    const enOverlap = Object.keys(sharedEn).filter((key) => key in pageEn);
    expect(
      enOverlap,
      'keys present in BOTH flat and page EN dictionaries (dead flat values — pick one layer)',
    ).toEqual([]);
    const zhOverlap = Object.keys(sharedZhCN).filter((key) => key in pageZh);
    expect(
      zhOverlap,
      'keys present in BOTH flat and page zh-CN dictionaries (dead flat values — pick one layer)',
    ).toEqual([]);
  });

  it('unflatten reverses dotted keys into nested dicts', () => {
    const nested = unflatten({ 'a.b.c': 'x', 'a.b.d': 'y', top: 'z' });
    expect(nested).toEqual({
      a: { b: { c: 'x', d: 'y' } },
      top: 'z',
    });
  });

  it('pairing-code placeholder states the exact leave-empty condition (C5)', () => {
    // CLIENT-P3-01 (3.8flash C5): the old value read as a generically
    // optional field ("optional if pack includes it") and users left it
    // empty without a client token in the pack, then hit the bare
    // INVALID_PAIRING_CODE rejection. The copy must state the precise
    // condition; pinned in BOTH locales so a regression to a vaguer
    // wording goes red.
    expect(en['sites.import_pairing_code_placeholder']).toBe(
      'Pairing code (leave empty only if the pack file includes a client token)',
    );
    expect(zh['sites.import_pairing_code_placeholder']).toBe(
      '配对码（仅当连接包文件内含 client token 时才可留空）',
    );
  });
});
