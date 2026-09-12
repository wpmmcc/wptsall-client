// catalog: WEBUI-UI-App
// oracle: L2
// 状态矩阵：P0-LF-04 本地 UI 边界：legacy 仅 Products 页 / Sidebar 无权益查询 / 页级 tab 门控 / 无付费分支控件。
import { describe, expect, it } from 'vitest';
import appSource from './App.svelte?raw';
import sidebarSource from './lib/components/Sidebar.svelte?raw';
import componentsSource from './pages/Components.svelte?raw';
import apiKeysSource from './pages/ApiKeys.svelte?raw';
import overviewSource from './pages/Overview.svelte?raw';

/**
 * P0-LF-04 local-boundary canaries.
 *
 * The default UI must not offer Products / Pro / login / entitlement or
 * server-template affordances, and must not issue server-control-plane
 * requests. Legacy surfaces stay available only behind an explicit
 * `runtime_mode === 'legacy_server_control_plane'` gate. These are static
 * source assertions because rendering App.svelte would mount every page.
 */

describe('P0-LF-04 local UI boundaries', () => {
  it('App renders the legacy Products page only in legacy runtime mode', () => {
    // The branch must be guarded by the legacy-mode flag, not reachable by
    // navigating to `products` in the default local UI.
    expect(appSource).toMatch(/currentPage === 'products' && legacyMode/);
    expect(appSource).toMatch(
      /legacyMode = \$derived\(\$status\?\.runtime_mode === 'legacy_server_control_plane'\)/,
    );
  });

  it('Sidebar issues no entitlement lookup and hides Pro/session/logout locally', () => {
    // No entitlement request from the sidebar at all.
    expect(sidebarSource).not.toMatch(/entitlements/);
    expect(sidebarSource).not.toMatch(/hasPro/);
    // Products entry + session/logout footer only render in legacy mode.
    expect(sidebarSource).toMatch(/legacyOnly: true/);
    expect(sidebarSource).toMatch(/\{#if legacyMode\}[\s\S]*session_token_prefix[\s\S]*\{\/if\}/);
  });

  it('Components hides the server template tab and ApiKeys hides website inventories locally', () => {
    expect(componentsSource).toMatch(/legacyMode\s*\?\s*\[\['my'[\s\S]*'server'[\s\S]*\]/);
    expect(componentsSource).toMatch(/\{:else if legacyMode\}\s*\n?\s*<ServerTemplatesTab/);

    // Local tab list must not contain the official website inventories.
    expect(apiKeysSource).toMatch(/legacyMode\s*\?\s*\[[\s\S]*tab_wp_providers[\s\S]*tab_cloud_api_types/);
    expect(apiKeysSource).toMatch(/'wp_providers' && legacyMode/);
    expect(apiKeysSource).toMatch(/'cloud_api_types' && legacyMode/);

    // Overview refresh buttons hit /api/domains|components/refresh (legacy only).
    expect(overviewSource).toMatch(
      /\{#if \$status\?\.runtime_mode === 'legacy_server_control_plane'\}[\s\S]*refresh_domains[\s\S]*\{\/if\}/,
    );
  });

  it('no paid/free branch controls exist outside the legacy Products page', () => {
    const sources: Array<[string, string]> = [
      ['App.svelte', appSource],
      ['Sidebar.svelte', sidebarSource],
      ['Components.svelte', componentsSource],
      ['ApiKeys.svelte', apiKeysSource],
      ['Overview.svelte', overviewSource],
    ];
    for (const [name, text] of sources) {
      // plan_tier / purchase state must never control product behaviour.
      expect(text, name).not.toMatch(/plan_tier/);
      expect(text, name).not.toMatch(/\btier\b/);
      expect(text, name).not.toMatch(/entitlement/);
    }
  });
});
