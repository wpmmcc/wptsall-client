import { test, expect, type BrowserContext } from '@playwright/test'

// ─────────────────────────────────────────────────────────────────────────────
// Server→Client 响应加密适配验证 (RFC #183)
//
// 验证内容：
//   1. OAuth 登录流程 — token exchange 加密信封处理（IKM = code_verifier）
//   2. 登录后域名列表获取 — 请求携带 X-WPTSALL-Accept-Encrypted header
//   3. 登录后组件列表获取 — 同上
//   4. 确认所有数据正确解密并加载
// ─────────────────────────────────────────────────────────────────────────────

const CLIENT_BASE   = process.env.CLIENT_BASE ?? 'http://127.0.0.1:8977'
const DEMO_EMAIL    = process.env.DEMO_EMAIL ?? 'demo@wptsall.dev'
const DEMO_PASSWORD = process.env.DEMO_PASSWORD ?? 'demo'

// ─────────────────────────────────────────────────────────────────────────────
// Cloudflare Turnstile bypass (identical to real-translation.spec.ts)
// ─────────────────────────────────────────────────────────────────────────────
async function bypassTurnstile(context: BrowserContext) {
  await context.route('**challenges.cloudflare.com/turnstile/**', (route) => {
    route.fulfill({ status: 200, contentType: 'text/javascript', body: '' })
  })
  await context.route('**www.wpmm.cc/cdn-cgi/challenge-platform/**', (route) => {
    route.fulfill({ status: 200, contentType: 'text/javascript', body: '' })
  })
  await context.route('**turnstile**', (route) => {
    route.fulfill({ status: 200, contentType: 'text/javascript', body: '' })
  })
  context.on('page', (page) => {
    page.addInitScript(() => {
      // @ts-ignore
      window.turnstile = {
        render(_container: unknown, options: { callback?: (t: string) => void }) {
          setTimeout(() => options?.callback?.('mock-turnstile-token'), 100)
          return 'mock-widget-id'
        },
        reset() {},
        remove() {},
        getResponse() { return 'mock-turnstile-token' },
      }
    })
  })
}

// ─────────────────────────────────────────────────────────────────────────────
// Test suite
// ─────────────────────────────────────────────────────────────────────────────
test.describe.configure({ mode: 'serial' })

test.describe('RFC #183: Server→Client 响应加密适配验证', () => {
  test.beforeAll(async ({ request }) => {
    console.log('\n── beforeAll: 验证服务状态 ──')
    const res = await request.get(`${CLIENT_BASE}/api/status`)
    expect(res.ok()).toBe(true)
    console.log('  Client (:8977): OK')
    console.log('── beforeAll complete ──\n')
  })

  // ── Test 1: OAuth 登录 + 加密 token exchange ──────────────────────────────
  test('OAuth 登录：token exchange 加密信封处理正常', async ({ page, context, request }) => {
    // Force logout to start clean
    try {
      await request.post(`${CLIENT_BASE}/api/logout`)
      console.log('  Pre-test: logged out')
    } catch { /* ignore */ }

    // Verify logged out
    const preStatus = await (await request.get(`${CLIENT_BASE}/api/status`)).json()
    expect(preStatus?.data?.logged_in).toBe(false)
    console.log('  Confirmed: logged_in=false')

    // Setup Turnstile bypass
    await bypassTurnstile(context)

    // Navigate to client UI
    await page.goto(CLIENT_BASE)
    await page.waitForSelector('button, a', { timeout: 15_000 })

    // Open OAuth popup
    const [popup] = await Promise.all([
      page.waitForEvent('popup', { timeout: 30_000 }),
      page.locator('button, a').filter({ hasText: /通过浏览器登录|Login|Sign in/i }).first().click(),
    ])

    await popup.waitForLoadState('domcontentloaded')
    console.log(`  Popup URL: ${popup.url()}`)

    // Fill credentials
    await popup.waitForSelector('input[type="email"], input[name="email"]', { timeout: 20_000 })
    await popup.fill('input[type="email"], input[name="email"]', DEMO_EMAIL)
    await popup.fill('input[type="password"], input[name="password"]', DEMO_PASSWORD)
    await popup.waitForTimeout(600)

    // Set captcha token directly
    await popup.evaluate(() => {
      const f = document.querySelector('input[name="captchaToken"]') as HTMLInputElement | null
      if (f) f.value = 'mock-turnstile-token'
    })

    // Submit
    await popup.click('button[type="submit"]')
    console.log('  Submitted login form')

    // Wait for popup to close or redirect
    await Promise.race([
      popup.waitForEvent('close', { timeout: 30_000 }),
      popup.waitForURL('**/callback**', { timeout: 30_000 }),
    ]).catch(() => {
      console.warn('  Popup did not close within 30s — continuing')
    })

    // Wait for authenticated nav
    await page.waitForSelector('nav.min-h-screen', { timeout: 25_000 })
    console.log('  Main page nav appeared')

    // Confirm login via API
    const statusRes = await request.get(`${CLIENT_BASE}/api/status`)
    const status = await statusRes.json()
    const loggedIn = status?.data?.logged_in
    const tokenPrefix = status?.data?.session_token_prefix ?? '(none)'
    console.log(`  ✅ logged_in=${loggedIn}, session_token_prefix=${tokenPrefix}`)
    expect(loggedIn).toBe(true)
    expect(tokenPrefix).not.toBe('(none)')
  })

  // ── Test 2: 域名列表获取（加密响应解密验证）────────────────────────────────
  test('域名列表：刷新域名验证加密解密流程正常', async ({ request }) => {
    // Refresh domains — this calls GET /api/v1/client/domains with
    // X-WPTSALL-Accept-Encrypted: true header
    console.log('\n  POST /api/domains/refresh...')
    const refreshRes = await request.post(`${CLIENT_BASE}/api/domains/refresh`)
    const refreshData = await refreshRes.json()
    console.log(`  Response: success=${refreshData?.success}`)
    expect(refreshData?.success).toBe(true)

    // Verify domains are loaded in status
    const statusRes = await request.get(`${CLIENT_BASE}/api/status`)
    const status = await statusRes.json()
    const domains = status?.data?.domains ?? []
    console.log(`  Domains loaded: ${domains.length}`)

    expect(domains.length).toBeGreaterThan(0)

    // Verify domain structure (proves successful JSON parsing after potential decryption)
    for (const d of domains) {
      expect(d.api_base_url).toBeTruthy()
      expect(typeof d.api_base_url).toBe('string')
      expect(d.license_status).toBeTruthy()
      console.log(`  → ${d.api_base_url} (license=${d.license_status}, plan=${d.plan_tier ?? 'n/a'})`)
    }

    // blog.wpmm.cc must be present
    const blogDomain = domains.find((d: { api_base_url: string }) => d.api_base_url.includes('blog.wpmm.cc'))
    expect(blogDomain).toBeTruthy()
    console.log('  ✅ 域名列表加密解密验证通过')
  })

  // ── Test 3: 组件列表获取（加密响应解密验证）────────────────────────────────
  test('组件列表：刷新组件验证加密解密流程正常', async ({ request }) => {
    // Refresh components — calls GET /api/v1/client/components with
    // X-WPTSALL-Accept-Encrypted: true header
    console.log('\n  POST /api/components/refresh...')
    const refreshRes = await request.post(`${CLIENT_BASE}/api/components/refresh`)
    const refreshData = await refreshRes.json()
    console.log(`  Response: success=${refreshData?.success}`)
    expect(refreshData?.success).toBe(true)

    // Verify components are loaded in status
    const statusRes = await request.get(`${CLIENT_BASE}/api/status`)
    const status = await statusRes.json()
    const components = status?.data?.components ?? []
    console.log(`  Components loaded: ${components.length}`)

    expect(components.length).toBeGreaterThan(0)

    // Verify component structure
    for (const c of components) {
      expect(c.id).toBeTruthy()
      expect(typeof c.id).toBe('string')
      expect(c.type).toBeTruthy()
      console.log(`  → ${c.id} (type=${c.type}, owner=${c.owner_type})`)
    }

    // At least one mock-sign or official component should exist (Server seeds these)
    const hasKnown = components.some((c: { id: string }) =>
      c.id.startsWith('mock-sign-') || c.id.startsWith('official-')
    )
    console.log(`  Has known component: ${hasKnown}`)
    expect(hasKnown).toBe(true)
    console.log('  ✅ 组件列表加密解密验证通过')
  })

  // ── Test 4: 日志验证 — 无解密错误 ─────────────────────────────────────────
  test('日志验证：确认无解密相关错误', async ({ request }) => {
    console.log('\n  Fetching recent log entries...')
    const logsRes = await request.post(`${CLIENT_BASE}/api/logs/recent`, {
      data: { limit: 50 },
    })
    const logs = await logsRes.json()
    const lines: string[] = logs?.data?.lines ?? []

    // Parse log entries
    const entries: Array<{ event?: string; level?: string; detail?: Record<string, unknown> }> = []
    for (const line of lines) {
      try { entries.push(JSON.parse(line)) } catch { /* skip non-JSON */ }
    }
    console.log(`  Parsed ${entries.length} log entries`)

    // Check for any decryption errors
    const decryptErrors = entries.filter(e =>
      e.level === 'error' &&
      (
        (typeof e.event === 'string' && e.event.includes('decrypt')) ||
        (typeof e.detail === 'object' && JSON.stringify(e.detail).includes('decrypt'))
      )
    )

    if (decryptErrors.length > 0) {
      console.error('  ❌ Decryption errors found:')
      for (const err of decryptErrors) {
        console.error(`    [${err.level}] ${err.event} — ${JSON.stringify(err.detail).slice(0, 200)}`)
      }
    } else {
      console.log('  ✅ 无解密相关错误')
    }
    expect(decryptErrors.length).toBe(0)

    // Check for auth-related errors (session issues that might indicate encryption problems)
    const authErrors = entries.filter(e =>
      e.level === 'error' &&
      typeof e.event === 'string' &&
      (e.event.includes('auth.') || e.event.includes('oauth.'))
    )
    if (authErrors.length > 0) {
      console.warn('  ⚠ Auth errors found (may indicate encryption issues):')
      for (const err of authErrors) {
        console.warn(`    [${err.level}] ${err.event}`)
      }
    }

    // Print last 10 entries for visibility
    console.log('\n  Last 10 log entries:')
    for (const e of entries.slice(-10)) {
      const detail = e.detail ? ` — ${JSON.stringify(e.detail).slice(0, 120)}` : ''
      console.log(`    [${e.level ?? '?'}] ${e.event ?? '?'}${detail}`)
    }

    console.log('  ✅ 日志验证完成')
  })
})
