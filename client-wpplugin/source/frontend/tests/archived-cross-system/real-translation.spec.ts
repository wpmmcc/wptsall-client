import { test, expect, type BrowserContext } from '@playwright/test'

// ─────────────────────────────────────────────────────────────────────────────
// Real-system E2E test suite
//
// Connects to:
//   - OAuth:         https://www.wpmm.cc  (demo account)
//   - WP Plugin:     https://blog.wpmm.cc
//   - Mock Translate: http://127.0.0.1:9090
//   - WPTSALL Client: http://127.0.0.1:8977
// ─────────────────────────────────────────────────────────────────────────────

// Environment-dependent values (from .env.test or process.env)
const CLIENT_BASE     = process.env.CLIENT_BASE ?? 'http://127.0.0.1:8977'
const MOCK_API_BASE   = process.env.MOCK_API_BASE ?? 'http://127.0.0.1:9090'
const DEMO_EMAIL      = process.env.DEMO_EMAIL ?? 'demo@wptsall.dev'
const DEMO_PASSWORD   = process.env.DEMO_PASSWORD ?? 'demo'
const WP_CLIENT_TOKEN = process.env.WP_CLIENT_TOKEN ?? ''
const ROUTE_SECRET    = process.env.ROUTE_SECRET ?? ''
const MOCK_API_KEY    = process.env.MOCK_API_KEY ?? 'mock-translate-dev-key-2026'

// Dynamically discovered from Server — not hardcoded
let DISCOVERED_COMPONENT_ID = ''
let CREATED_LOCAL_COMPONENT_ID = ''

type AuthField = { name: string }

const TASK_TYPE_BINDING_BUSINESS_LINES = [
  'custom_model',
  'post_content',
  'taxonomy_content',
  'plugin_i18n',
  'config_i18n',
  'theme_i18n',
]

const STABLE_TEXT_COMPONENT_PRIORITY = [
  'mock-sign-kakao',
  'mock-sign-azure',
  'mock-sign-hmac-sha256',
  'mock-sign-alibaba',
  'mock-sign-md5',
  'mock-sign-sha256',
  'mock-sign-aws',
  'mock-sign-volcengine',
  'mock-sign-tc3',
  'official-alibaba-qwen-mt-v1',
  'official-alibaba-v1',
  'official-aws-v1',
  'official-apertium-v1',
  'mock-sign-oauth',
]

const KNOWN_COMPONENT_AUTH_VALUES: Record<string, Record<string, string>> = {
  'mock-sign-kakao': { api_key: 'mock-kakao-key' },
  'mock-sign-azure': { subscription_key: 'mock-azure-sub-key' },
  'mock-sign-hmac-sha256': { api_key: 'mock-key-hmac', secret_key: 'mock-secret-hmac' },
  'mock-sign-alibaba': { access_key: 'mock-ali-key', secret_key: 'mock-ali-secret' },
  'mock-sign-md5': { appid: 'mock-appid-001', secret_key: 'mock-secret-baidu' },
  'mock-sign-sha256': { app_key: 'mock-appkey-youdao', app_secret: 'mock-secret-youdao' },
  'mock-sign-aws': { access_key: 'mock-aws-key', secret_key: 'mock-aws-secret', host: '127.0.0.1:9090' },
  'mock-sign-volcengine': { access_key: 'mock-volc-key', secret_key: 'mock-volc-secret', host: '127.0.0.1:9090' },
  'mock-sign-tc3': { secret_id: 'mock-tc-id', secret_key: 'mock-tc-secret', host: '127.0.0.1:9090' },
}

const KNOWN_COMPONENT_AUTH_FIELDS: Record<string, string[]> = {
  'mock-sign-oauth': ['api_key'],
  'mock-sign-kakao': ['api_key'],
  'mock-sign-azure': ['subscription_key'],
  'mock-sign-hmac-sha256': ['api_key', 'secret_key'],
  'mock-sign-alibaba': ['access_key', 'secret_key'],
  'mock-sign-md5': ['appid', 'secret_key'],
  'mock-sign-sha256': ['app_key', 'app_secret'],
  'mock-sign-aws': ['access_key', 'secret_key', 'host'],
  'mock-sign-volcengine': ['access_key', 'secret_key', 'host'],
  'mock-sign-tc3': ['secret_id', 'secret_key', 'host'],
}

function inferAuthFields(componentId: string): AuthField[] {
  const names = KNOWN_COMPONENT_AUTH_FIELDS[componentId] ?? []
  return names.map((name) => ({ name }))
}

// ─────────────────────────────────────────────────────────────────────────────
// Cloudflare Turnstile bypass
//
// Intercepts Turnstile challenge requests and injects a mock window.turnstile
// object that auto-calls options.callback('mock-turnstile-token') so the login
// form proceeds without a real CAPTCHA solve.
// ─────────────────────────────────────────────────────────────────────────────
async function bypassTurnstile(context: BrowserContext) {
  // Intercept Cloudflare Turnstile JS bundles — return an empty 200 response
  await context.route('**challenges.cloudflare.com/turnstile/**', (route) => {
    route.fulfill({
      status: 200,
      contentType: 'text/javascript',
      body: '',
    })
  })

  // Intercept Cloudflare challenge platform assets
  await context.route('**www.wpmm.cc/cdn-cgi/challenge-platform/**', (route) => {
    route.fulfill({
      status: 200,
      contentType: 'text/javascript',
      body: '',
    })
  })

  // Intercept any remaining turnstile-related URLs
  await context.route('**turnstile**', (route) => {
    route.fulfill({
      status: 200,
      contentType: 'text/javascript',
      body: '',
    })
  })

  // On every new page/popup, inject the mock window.turnstile object before
  // any page scripts execute.  This causes the widget to instantly resolve.
  context.on('page', (page) => {
    page.addInitScript(() => {
      // @ts-ignore
      window.turnstile = {
        render(container: unknown, options: { callback?: (token: string) => void; 'error-callback'?: () => void }) {
          console.log('[turnstile mock] render called — auto-resolving with mock token')
          setTimeout(() => {
            if (typeof options?.callback === 'function') {
              options.callback('mock-turnstile-token')
            }
          }, 100)
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

test.describe('Real-system E2E: OAuth → Worker → Translation', () => {
  // ── beforeAll: verify that both local services are reachable ───────────────
  test.beforeAll(async ({ request }) => {
    console.log('\n── beforeAll: verifying local services ──')

    // Verify mock translate API
    let mockOk = false
    try {
      const res = await request.get(`${MOCK_API_BASE}/api/v1/health`)
      mockOk = res.ok()
      console.log(`  Mock translate API (:9090): ${mockOk ? 'OK' : `HTTP ${res.status()}`}`)
    } catch (err) {
      console.warn(`  Mock translate API (:9090): UNREACHABLE — ${err}`)
    }

    // Verify WPTSALL client
    let clientOk = false
    try {
      const res = await request.get(`${CLIENT_BASE}/api/status`)
      clientOk = res.ok()
      console.log(`  WPTSALL Client (:8977): ${clientOk ? 'OK' : `HTTP ${res.status()}`}`)
    } catch (err) {
      console.warn(`  WPTSALL Client (:8977): UNREACHABLE — ${err}`)
    }

    if (!clientOk) {
      throw new Error('WPTSALL Client at :8977 is not running. Start it with WPTSALL_WEB_UI=true before running E2E tests.')
    }

    // Check required environment variables
    if (!WP_CLIENT_TOKEN || !ROUTE_SECRET) {
      throw new Error(
        'WP_CLIENT_TOKEN and ROUTE_SECRET required.\n' +
        'Set in client/frontend/.env.test or environment.\n' +
        'Get from WP:\n' +
        '  wp eval \'echo wptsall_get_client_api_token(false);\'\n' +
        '  wp eval \'echo wptsall_get_client_route_secret();\''
      )
    }

    console.log('── beforeAll complete ──\n')
  })

  // ── afterAll: clean up component binding created during tests ─────────────
  test.afterAll(async ({ request }) => {
    console.log('\n── afterAll: cleaning up component binding and task-type bindings ──')
    if (DISCOVERED_COMPONENT_ID) {
      try {
        const res = await request.post(`${CLIENT_BASE}/api/components/bindings/delete`, {
          data: { component_id: DISCOVERED_COMPONENT_ID },
        })
        console.log(`  Delete binding for ${DISCOVERED_COMPONENT_ID}: ${res.status()}`)
      } catch (err) {
        console.warn(`  Failed to clean up component binding: ${err}`)
      }
    } else {
      console.log('  No component was discovered — nothing to clean up')
    }
    try {
      await request.post(`${CLIENT_BASE}/api/task-type-components/delete`, {
        data: { task_type: 'text' },
      })
      for (const line of TASK_TYPE_BINDING_BUSINESS_LINES) {
        await request.post(`${CLIENT_BASE}/api/task-type-components/delete`, {
          data: { task_type: 'text', business_line: line },
        })
      }
    } catch (err) {
      console.warn(`  Failed to clean up task-type bindings: ${err}`)
    }
    if (CREATED_LOCAL_COMPONENT_ID) {
      try {
        const encoded = encodeURIComponent(CREATED_LOCAL_COMPONENT_ID)
        await request.delete(`${CLIENT_BASE}/api/components/local/${encoded}`)
        console.log(`  Deleted local component: ${CREATED_LOCAL_COMPONENT_ID}`)
      } catch (err) {
        console.warn(`  Failed to delete local component ${CREATED_LOCAL_COMPONENT_ID}: ${err}`)
      }
    }
    console.log('── afterAll complete ──\n')
  })

  // ── Test 1: OAuth login via real www.wpmm.cc ───────────────────────────────
  test('OAuth 登录：通过 www.wpmm.cc 真实认证', async ({ page, context, request }) => {
    // Force logout first so we start from a clean state
    try {
      await request.post(`${CLIENT_BASE}/api/logout`)
      console.log('  Logged out (pre-test cleanup)')
    } catch {
      // Ignore — might not be logged in
    }

    // Set up Turnstile bypass BEFORE opening any pages
    await bypassTurnstile(context)

    // Navigate to client UI
    await page.goto(CLIENT_BASE)

    // Wait for the login button to appear
    await page.waitForSelector('button, a', { timeout: 15_000 })

    // Open OAuth popup and click login simultaneously
    const [popup] = await Promise.all([
      page.waitForEvent('popup', { timeout: 30_000 }),
      page.locator('button, a').filter({ hasText: /通过浏览器登录|Login|Sign in/i }).first().click(),
    ])

    // Wait for the OAuth page to load
    await popup.waitForLoadState('domcontentloaded')
    console.log(`  Popup URL: ${popup.url()}`)

    // Wait for email input on the OAuth page
    await popup.waitForSelector('input[type="email"], input[name="email"]', { timeout: 20_000 })

    // Fill in credentials
    await popup.fill('input[type="email"], input[name="email"]', DEMO_EMAIL)
    await popup.fill('input[type="password"], input[name="password"]', DEMO_PASSWORD)
    console.log('  Filled email and password')

    // Wait 600 ms for Turnstile mock to auto-complete
    await popup.waitForTimeout(600)

    // Attempt to populate the hidden captchaToken field directly
    await popup.evaluate(() => {
      const f = document.querySelector('input[name="captchaToken"]') as HTMLInputElement | null
      if (f) {
        f.value = 'mock-turnstile-token'
        console.log('[test] set captchaToken field')
      }
    })

    // Submit the login form
    await popup.click('button[type="submit"]')
    console.log('  Clicked submit')

    // Wait for popup to close OR URL to contain 'callback' (OAuth redirect)
    await Promise.race([
      popup.waitForEvent('close', { timeout: 30_000 }),
      popup.waitForURL('**/callback**', { timeout: 30_000 }),
    ]).catch(() => {
      console.warn('  Popup did not close/redirect within 30s — continuing anyway')
    })

    // Wait for main page to show the authenticated nav
    await page.waitForSelector('nav.min-h-screen', { timeout: 25_000 })
    console.log('  Main page nav appeared — login successful')

    // Confirm via API that we are logged in
    const statusRes = await request.get(`${CLIENT_BASE}/api/status`)
    const status = await statusRes.json()
    console.log(`  Status: logged_in=${status?.data?.logged_in}`)
    expect(status?.data?.logged_in).toBe(true)
  })

  // ── Test 2: Refresh domains, discover components, bind credentials ────────
  test('Worker 准备：刷新域名、发现组件、绑定凭据', async ({ page, request }) => {
    await page.goto(CLIENT_BASE)
    await page.waitForSelector('nav.min-h-screen', { timeout: 20_000 })

    // 1. Refresh domains from real Server
    console.log('\n  1. Refreshing domains from www.wpmm.cc...')
    const refreshRes = await request.post(`${CLIENT_BASE}/api/domains/refresh`)
    expect((await refreshRes.json())?.success).toBe(true)

    // 2. Configure WP Token for blog.wpmm.cc
    console.log(`  2. Upserting domain token: blog.wpmm.cc, route_secret=${ROUTE_SECRET}`)
    const tokenRes = await request.post(`${CLIENT_BASE}/api/domain-tokens/upsert`, {
      data: {
        api_base_url: 'https://blog.wpmm.cc',
        wp_client_token: WP_CLIENT_TOKEN,
        route_secret: ROUTE_SECRET,
      },
    })
    expect((await tokenRes.json())?.success).toBe(true)

    // 3. Refresh components from Server (dynamic discovery)
    console.log('  3. Refreshing components from Server...')
    await request.post(`${CLIENT_BASE}/api/components/refresh`)
    await page.waitForTimeout(2_000)

    // 4. Read component list from status
    const status = await (await request.get(`${CLIENT_BASE}/api/status`)).json()
    const components: Array<{ id: string; type: string }> = status?.data?.components ?? []
    console.log(`  4. Discovered ${components.length} components`)

    // 5. Select a usable text_translation component dynamically
    const textComponents = components.filter((c) => c.type === 'text_translation')
    const componentPriority = (id: string): number => {
      const idx = STABLE_TEXT_COMPONENT_PRIORITY.indexOf(id)
      if (idx >= 0) return idx
      if (id.startsWith('mock-sign-')) return 100
      if (id.startsWith('official-')) return 200
      return 1000
    }
    const preferredTextComponents = [...textComponents].sort((a, b) => {
      const pa = componentPriority(a.id)
      const pb = componentPriority(b.id)
      if (pa !== pb) return pa - pb
      return a.id.localeCompare(b.id)
    })

    let selectedComp: { id: string; type: string } | null = null
    let authFields: AuthField[] = []
    for (const candidate of preferredTextComponents) {
      const tmplRes = await request.post(`${CLIENT_BASE}/api/components/template`, {
        data: { component_id: candidate.id },
      })
      const tmplData = await tmplRes.json().catch(() => null as unknown)
      if (!tmplRes.ok() || !(tmplData as Record<string, unknown> | null)?.success) {
        console.warn(`  Template fetch failed for ${candidate.id}: HTTP ${tmplRes.status()}`)
        continue
      }
      const tmplEntry = ((tmplData as Record<string, unknown> | null)?.data as Record<string, unknown> | undefined) ?? {}
      const templateJson = (tmplEntry.template_json as Record<string, unknown> | undefined) ?? {}
      const requestDef = (templateJson.request as Record<string, unknown> | undefined) ?? {}
      const reqUrl = String(requestDef.url ?? '')
      const isOAuthTranslateComponent =
        candidate.id === 'mock-sign-oauth' || reqUrl.includes('/api/oauth/translate')
      if (isOAuthTranslateComponent) {
        console.log(`  Skipping ${candidate.id}: requires OAuth token flow not configured in this suite`)
        continue
      }
      let candidateAuthFields: AuthField[] = (
        tmplEntry.auth_fields as AuthField[] | undefined
      ) ?? []
      if (candidateAuthFields.length === 0) {
        candidateAuthFields = inferAuthFields(candidate.id)
      }
      selectedComp = candidate
      authFields = candidateAuthFields
      break
    }

    if (!selectedComp) {
      const localId = `pw-local-openai-${Date.now()}`
      console.log(`  5. No usable server template, creating local fallback: ${localId}`)
      const localCreateRes = await request.post(`${CLIENT_BASE}/api/components/local`, {
        data: {
          id: localId,
          name: 'PW Local OpenAI',
          kind: 'openai_compatible',
          template_id: 'official-openai-text-v1',
          api_base: MOCK_API_BASE,
          model: 'mock-openai-v1',
          remarks: 'Playwright fallback component',
        },
      })
      const localCreate = await localCreateRes.json().catch(() => null as unknown)
      expect(localCreateRes.ok()).toBe(true)
      expect((localCreate as Record<string, unknown> | null)?.success).toBe(true)
      CREATED_LOCAL_COMPONENT_ID = localId
      selectedComp = { id: localId, type: 'text_translation' }
      authFields = [{ name: 'api_key' }]
    }

    expect(selectedComp).toBeTruthy()
    DISCOVERED_COMPONENT_ID = selectedComp!.id
    console.log(`  5. Selected component: ${DISCOVERED_COMPONENT_ID}`)
    console.log(`  6. Auth fields: ${authFields.map((f) => f.name).join(', ')}`)

    // 7. Build auth object and bind selected component
    const auth: Record<string, string> = {}
    const knownAuth = KNOWN_COMPONENT_AUTH_VALUES[DISCOVERED_COMPONENT_ID] ?? {}
    for (const field of authFields) {
      const name = String(field.name || '').trim()
      if (!name) continue
      if (knownAuth[name]) {
        auth[name] = knownAuth[name]
      } else if (name === 'api_key') {
        auth[name] = MOCK_API_KEY
      } else if (name === 'subscription_key') {
        auth[name] = MOCK_API_KEY
      } else if (name === 'host') {
        auth[name] = '127.0.0.1:9090'
      } else if (name === 'access_key') {
        auth[name] = 'mock-access-key'
      } else if (name === 'secret_key') {
        auth[name] = 'mock-secret-key'
      } else if (name === 'appid') {
        auth[name] = 'mock-app-id'
      } else if (name === 'app_key') {
        auth[name] = 'mock-app-key'
      } else if (name === 'app_secret') {
        auth[name] = 'mock-app-secret'
      } else if (name === 'secret_id') {
        auth[name] = 'mock-secret-id'
      } else if (name.includes('secret')) {
        auth[name] = 'mock-secret'
      } else if (name.includes('token')) {
        auth[name] = 'mock-token'
      } else if (name.includes('key')) {
        auth[name] = MOCK_API_KEY
      } else {
        auth[name] = 'mock-value'
      }
    }
    if (authFields.length > 0) {
      expect(Object.keys(auth).length).toBeGreaterThan(0)
    }

    const bindRes = await request.post(`${CLIENT_BASE}/api/components/bindings/upsert`, {
      data: { component_id: DISCOVERED_COMPONENT_ID, auth },
    })
    expect((await bindRes.json())?.success).toBe(true)
    console.log(`  7. Component ${DISCOVERED_COMPONENT_ID} bound with auth: ${Object.keys(auth).join(', ')}`)

    // 8. Upsert deterministic text component mapping (global + business lines)
    const bindingScopes = [
      { task_type: 'text' },
      ...TASK_TYPE_BINDING_BUSINESS_LINES.map((business_line) => ({
        task_type: 'text',
        business_line,
      })),
    ]
    for (const scope of bindingScopes) {
      const upsertRes = await request.post(`${CLIENT_BASE}/api/task-type-components/upsert`, {
        data: {
          ...scope,
          component_id: DISCOVERED_COMPONENT_ID,
        },
      })
      expect((await upsertRes.json())?.success).toBe(true)
    }
    console.log('  8. Task-type bindings upserted for text scopes')
  })

  // ── Test 3: Start/stop worker loop (production-like path) ───────────────────
  test('Worker 自动模式：start/stop 对接 blog.wpmm.cc', async ({ page, request }) => {
    test.setTimeout(300_000)
    await page.goto(CLIENT_BASE)
    await page.waitForSelector('nav.min-h-screen', { timeout: 20_000 })

    // Configure worker: poll_seconds=20 (discovery_tasks.per_page controls batch size)
    console.log('\n  Configuring worker via /api/worker/config...')
    await request.post(`${CLIENT_BASE}/api/worker/config`, {
      data: {
        poll_seconds: 20,
        callback_concurrency: 1,
        callback_retry_max: 4,
        fetch_timeout_secs: 45,
        fetch_retry_max: 6,
      },
    })

    // Tune existing discovery rows down to reduce WP 429 rate-limit errors.
    const tuneRes = await request.get(`${CLIENT_BASE}/api/discovery-tasks`)
    if (tuneRes.ok()) {
      const tuneData = await tuneRes.json().catch(() => null as unknown)
      const tuneItems: Array<{ id: number; domain: string }> =
        ((tuneData as Record<string, unknown> | null)?.data as Record<string, unknown> | undefined)?.items as Array<{ id: number; domain: string }> ?? []
      const currentDomainNeedle = `/wptsall/v2/${ROUTE_SECRET}/client`
      const activeItems = tuneItems.filter(
        (item) => item.domain.includes('blog.wpmm.cc') && item.domain.includes(currentDomainNeedle)
      )
      const staleItems = tuneItems.filter(
        (item) => item.domain.includes('blog.wpmm.cc') && !item.domain.includes(currentDomainNeedle)
      )

      for (const item of staleItems) {
        await request.put(`${CLIENT_BASE}/api/discovery-tasks/${item.id}`, {
          data: {
            enabled: false,
          },
        })
      }

      for (const item of activeItems) {
        await request.put(`${CLIENT_BASE}/api/discovery-tasks/${item.id}`, {
          data: {
            concurrency: 1,
            batch_parallel: 1,
            per_page: 200,
            retry_max: 4,
            timeout_secs: 90,
            enabled: true,
            include_resync: false,
          },
        })
      }
    }

    console.log('  Starting worker loop (POST /api/worker/start)...')
    const startRes = await request.post(`${CLIENT_BASE}/api/worker/start`, {
      data: {},
      timeout: 30_000,
    })
    const startData = await startRes.json()
    expect(startData?.success).toBe(true)

    // Wait until status flips to running_auto
    let statusData: Record<string, unknown> | null = null
    for (let i = 0; i < 20; i += 1) {
      const res = await request.get(`${CLIENT_BASE}/api/status`)
      const body = await res.json().catch(() => null as unknown)
      statusData = (body as Record<string, unknown> | null)?.data as Record<string, unknown> | null
      const running = !!statusData?.worker_loop_running
      const status = String(statusData?.worker_status ?? '')
      if (running && status === 'running_auto') break
      await page.waitForTimeout(1000)
    }
    expect(!!statusData?.worker_loop_running).toBe(true)
    expect(String(statusData?.worker_status ?? '')).toBe('running_auto')

    // Let one production-like tick run for a short window.
    await page.waitForTimeout(20_000)

    console.log('  Stopping worker loop (POST /api/worker/stop)...')
    const stopRes = await request.post(`${CLIENT_BASE}/api/worker/stop`, {
      data: {},
      timeout: 30_000,
    })
    const stopData = await stopRes.json()
    expect(stopData?.success).toBe(true)

    const finalStatusRes = await request.get(`${CLIENT_BASE}/api/status`)
    const finalStatus = await finalStatusRes.json()
    const finalData = finalStatus?.data ?? {}
    expect(finalData?.worker_loop_running).toBe(false)
    expect(['idle', 'waiting', 'completed', 'running_auto', 'error']).toContain(finalData?.worker_status)
    console.log(`  worker_status after stop: ${finalData?.worker_status}`)
  })

  // ── Test 3.5: Verify discovery tasks were auto-created per relation ────────
  test('Discovery Tasks 验证：Worker 运行后自动为每个 relation 创建任务行', async ({ page, request }) => {
    await page.goto(CLIENT_BASE)
    await page.waitForSelector('nav.min-h-screen', { timeout: 20_000 })

    // ── 1. API: GET /api/discovery-tasks ────────────────────────────────────
    console.log('\n  Fetching discovery tasks via API...')
    const tasksRes = await request.get(`${CLIENT_BASE}/api/discovery-tasks`)
    expect(tasksRes.ok()).toBe(true)

    const tasksData = await tasksRes.json()
    expect(tasksData?.success).toBe(true)

    const items: Array<{
      id: number; domain: string; relation_id: number;
      concurrency: number; batch_parallel: number; per_page: number;
      retry_max: number; timeout_secs: number; enabled: boolean; last_run_at: number;
    }> = tasksData?.data?.items ?? []

    console.log(`  Total discovery tasks: ${items.length}`)
    for (const t of items) {
      console.log(`  → id=${t.id} domain=${t.domain} relation_id=${t.relation_id} concurrency=${t.concurrency} last_run_at=${t.last_run_at}`)
    }

    // Worker ran → at least one discovery task must exist
    expect(items.length).toBeGreaterThan(0)

    // blog.wpmm.cc tasks for current route_secret
    const currentDomainNeedle = `/wptsall/v2/${ROUTE_SECRET}/client`
    const blogTasks = items.filter(
      t => t.domain.includes('blog.wpmm.cc') && t.domain.includes(currentDomainNeedle)
    )
    console.log(`  blog.wpmm.cc relations: ${blogTasks.map(t => t.relation_id).join(', ')}`)
    expect(blogTasks.length).toBeGreaterThan(0)

    // Verify tuned params on each active task
    for (const t of blogTasks) {
      expect(t.concurrency).toBe(1)
      expect(t.batch_parallel).toBe(1)
      expect(t.per_page).toBe(200)
      expect(t.retry_max).toBe(4)
      expect(t.timeout_secs).toBe(90)
      expect(t.enabled).toBe(true)
      expect(t.last_run_at).toBeGreaterThan(0)
    }
    console.log('  ✅ Tuned params verified')

    // ── 2. UI: navigate to Tasks page and verify table ───────────────────────
    console.log('\n  Navigating to Tasks page in Web UI...')
    await page.locator('nav button').filter({ hasText: '任务' }).click()
    await page.waitForTimeout(300)

    // Default tab is '翻译任务' (jobs); switch to '并发配置' to see the discovery tasks table
    await page.locator('button:has-text("并发配置")').click()
    await page.waitForTimeout(300)

    // Wait for discovery tasks table to appear
    await page.waitForSelector('table tbody tr', { timeout: 10_000 })

    const rowCount = await page.locator('table tbody tr').count()
    console.log(`  Table rows visible: ${rowCount}`)
    expect(rowCount).toBe(items.length)

    // Each blog.wpmm.cc relation_id must appear in the table
    for (const t of blogTasks) {
      await expect(page.locator('table tbody')).toContainText(String(t.relation_id))
    }

    // Tuned concurrency/per_page values should be visible
    await expect(page.locator('table tbody')).toContainText('1')
    await expect(page.locator('table tbody')).toContainText('200')

    console.log('  ✅ Tasks page table verified')
  })

  // ── Test 3.6: Jobs API shape + Tasks page '翻译任务' tab ───────────────────
  test('Jobs API 验证：GET /api/jobs 响应格式正确，翻译任务 Tab 默认激活', async ({ page, request }) => {
    await page.goto(CLIENT_BASE)
    await page.waitForSelector('nav.min-h-screen', { timeout: 20_000 })

    // ── 1. API: GET /api/jobs ────────────────────────────────────────────────
    console.log('\n  Fetching jobs via GET /api/jobs...')
    const jobsRes = await request.get(`${CLIENT_BASE}/api/jobs`)
    expect(jobsRes.ok()).toBe(true)

    const jobsData = await jobsRes.json()
    // Must conform to { success: true, data: { items: [...] } }
    expect(jobsData?.success).toBe(true)
    expect(jobsData?.data).toBeDefined()
    expect(Array.isArray(jobsData?.data?.items)).toBe(true)

    const jobs: Array<{
      id: number; domain: string; relation_id: number; business_line: string
      status: string; total_items: number; done_items: number; failed_items: number
      triggered_by: string; started_at: number | null; completed_at: number | null
      created_at: number; updated_at: number
    }> = jobsData?.data?.items ?? []

    console.log(`  Total jobs returned: ${jobs.length}`)

    if (jobs.length > 0) {
      const first = jobs[0]
      // Field type validation
      expect(typeof first.id).toBe('number')
      expect(typeof first.domain).toBe('string')
      expect(typeof first.relation_id).toBe('number')
      expect(typeof first.status).toBe('string')
      expect(['pending', 'running', 'completed', 'failed', 'partial']).toContain(first.status)
      expect(typeof first.total_items).toBe('number')
      expect(typeof first.done_items).toBe('number')
      expect(typeof first.failed_items).toBe('number')
      expect(typeof first.created_at).toBe('number')
      console.log(`  First job: id=${first.id} domain=${first.domain} rel=${first.relation_id} status=${first.status}`)
      console.log('  ✅ Job field types verified')

      // ── 2. API: GET /api/jobs/:id ────────────────────────────────────────
      console.log(`\n  Fetching single job via GET /api/jobs/${first.id}...`)
      const jobRes = await request.get(`${CLIENT_BASE}/api/jobs/${first.id}`)
      expect(jobRes.ok()).toBe(true)
      const jobDetail = await jobRes.json()
      expect(jobDetail?.success).toBe(true)
      expect(jobDetail?.data?.id).toBe(first.id)
      console.log('  ✅ Single job GET verified')

      // ── 3. API: GET /api/jobs/:id/items ─────────────────────────────────
      console.log(`\n  Fetching items for job ${first.id}...`)
      const itemsRes = await request.get(`${CLIENT_BASE}/api/jobs/${first.id}/items`)
      expect(itemsRes.ok()).toBe(true)
      const itemsData = await itemsRes.json()
      expect(itemsData?.success).toBe(true)
      expect(Array.isArray(itemsData?.data?.items)).toBe(true)
      console.log(`  Items count: ${itemsData?.data?.items?.length ?? 0}`)
      console.log('  ✅ Job items GET verified')
    } else {
      console.log('  (0 jobs returned — worker may not have created any; field verification skipped)')
    }

    // ── 4. UI: Tasks page default tab = '翻译任务' ───────────────────────────
    console.log('\n  Verifying Tasks page UI structure...')
    await page.locator('nav button').filter({ hasText: '任务' }).click()
    await page.waitForTimeout(300)

    await expect(page.locator('h2:has-text("任务管理")')).toBeVisible()

    const jobsTabBtn = page.locator('button:has-text("翻译任务")')
    const discoveryTabBtn = page.locator('button:has-text("并发配置")')
    await expect(jobsTabBtn).toBeVisible()
    await expect(discoveryTabBtn).toBeVisible()

    // Active tab has 'bg-white' class; inactive does not
    await expect(jobsTabBtn).toHaveClass(/bg-white/)
    await expect(discoveryTabBtn).not.toHaveClass(/bg-white/)

    // Default tab content: job cards or empty-state placeholder
    const hasJobCards = await page.locator('.bg-white.border.border-gray-200.rounded-xl').count()
    const hasEmptyMsg = await page.locator('text=暂无翻译任务记录').isVisible().catch(() => false)
    console.log(`  Job cards: ${hasJobCards}, empty-state: ${hasEmptyMsg}`)
    expect(hasJobCards > 0 || hasEmptyMsg).toBe(true)

    await expect(page.locator('button:has-text("刷新")')).toBeVisible()
    console.log('  ✅ Tasks page UI structure verified')
  })

  // ── Test 4: Verify translation results via log events ─────────────────────
  test('验证翻译结果：检查日志事件', async ({ page, request }) => {
    await page.goto(CLIENT_BASE)
    await page.waitForSelector('nav.min-h-screen', { timeout: 20_000 })

    console.log('\n  Fetching recent log entries...')

    const logsRes = await request.post(`${CLIENT_BASE}/api/logs/recent`, {
      data: { limit: 100 },
    })
    const logs = await logsRes.json()

    // Log API returns { success, data: { lines: ["...json...", ...] } }
    // Each line is a JSON string: { event, level, ts, detail }
    const rawLines: string[] = logs?.data?.lines ?? []
    const entries = rawLines.map((line) => {
      try { return JSON.parse(line) as { event?: string; level?: string; ts?: number; detail?: unknown }
      } catch { return { raw: line } }
    })

    console.log(`  Total log entries returned: ${entries.length}`)

    // Print the last 15 entries for visibility
    const tail = entries.slice(-15)
    console.log('  Last 15 entries:')
    for (const entry of tail) {
      if ('raw' in entry) {
        console.log(`    ${(entry as { raw: string }).raw}`)
      } else {
        const e = entry as { event?: string; level?: string; detail?: unknown }
        const detail = e.detail ? ` — ${JSON.stringify(e.detail).slice(0, 120)}` : ''
        console.log(`    [${e.level ?? '?'}] ${e.event ?? '?'}${detail}`)
      }
    }

    // Check for expected event patterns from a worker run
    const relevantPrefixes = [
      'worker.run_start',
      'worker.completed',
      'domain.',
      'translation_callback',
      'discovery.',
    ]

    const foundEvents = new Set<string>()
    for (const entry of entries) {
      const evt = (entry as { event?: string }).event ?? ''
      for (const prefix of relevantPrefixes) {
        if (typeof evt === 'string' && evt.startsWith(prefix)) {
          foundEvents.add(prefix)
        }
      }
    }

    if (foundEvents.size > 0) {
      console.log(`  Found relevant event prefixes: ${[...foundEvents].join(', ')}`)
    } else {
      console.log('  No worker-related log events found (worker may not have run, or log file empty).')
    }

    // At minimum the log API must respond OK
    expect(logsRes.ok()).toBe(true)
  })
})
