/**
 * Private rendered review fixture. This serves the real shared Svelte page,
 * locale dictionaries and Desktop adapter. HTTP, provider and Tauri outcomes
 * are owned mock contracts, not a native GUI or paid-provider proof.
 */
import { createServer } from '../../../../../client-wpplugin/source/frontend/node_modules/vite/dist/node/index.js';
import { svelte } from '../../../../../client-wpplugin/source/frontend/node_modules/@sveltejs/vite-plugin-svelte/src/index.js';
import tailwindcss from '../../../../../client-wpplugin/source/frontend/node_modules/@tailwindcss/vite/dist/index.mjs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import fs from 'node:fs';
import { createServer as createNetServer } from 'node:net';

const root = fileURLToPath(new URL('../../../../../', import.meta.url));
const frontend = path.join(root, 'client-wpplugin/source/frontend');
const output = process.argv[2];
if (!output || !path.isAbsolute(output) || fs.existsSync(output)) {
  throw new Error('A new absolute metadata file is required');
}
const models = new Map();
const request = '631b8b74-913a-4972-8892-33a892ce07bb';

function newModel(mode) {
  const model = {
    item: {
      id: 8, job_id: 1, domain: 'https://owned.invalid', relation_id: 77,
      business_line: 'plugin_i18n', object_type: 'language_pack',
      wp_object_id: 81, wp_object_subtype: 'plugin', task_type: 'text',
      source_lang: 'en', target_lang: 'zh', component_id: 'owned-pack',
      selected_component_id: 'owned-pack', raw_path: '/owned/raw.wptc',
      translated_path: '/owned/translated.wptc', status: 'pending_review',
      client_task_id: 'owned-pack-review', upload_id: null, wp_attachment_id: null,
      error_message: null, retry_count: 0, max_retries: 3, fetched_at: null,
      translated_at: 1, synced_at: null, created_at: 1, updated_at: 1,
    },
    raw: {
      relation_id: 77, business_line: 'plugin_i18n', subtype: 'plugin',
      source_lang: 'en', target_lang: 'zh',
      entries: [
        { entry_id: 101, msgstr: 'Old button %s', source: {
          object_id: 81, text_domain: 'owned-pack', msgctxt: 'Owned button context',
          msgid: 'Hello %s', msgid_plural: 'Hello %s items', plural_index: 1,
        } },
        { entry_id: 102, msgstr: 'Old menu %s', source: {
          object_id: 82, text_domain: 'owned-pack', msgctxt: 'Owned menu context',
          msgid: 'Hello %s', msgid_plural: '', plural_index: 0,
        } },
      ],
    },
    translated: { payload: {
      relation_id: 77, business_line: 'plugin_i18n', source_lang: 'en', target_lang: 'zh',
      entries: [{ entry_id: 102, msgstr: 'Menu %s' }, { entry_id: 101, msgstr: 'Button %s' }],
    } },
    manual_request_id: mode === 'manual' ? request : null,
    delivery_unresolved: mode === 'delivery',
    calls: [], invokes: [], writebacks: [], feeSubmits: 0,
  };
  if (mode === 'invalid') model.raw.entries[1].entry_id = 101;
  return model;
}

function scope(req) {
  const url = new URL(req.headers.referer ?? 'http://127.0.0.1/?case=default');
  const key = url.searchParams.get('case') ?? 'default';
  const mode = url.searchParams.get('mode') ?? 'edit';
  if (!models.has(key)) models.set(key, newModel(mode));
  return { key, mode, model: models.get(key) };
}

function response(res, body, status = 200) {
  res.statusCode = status;
  res.setHeader('Content-Type', 'application/json');
  res.end(JSON.stringify(body));
}

async function readBody(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  const bytes = Buffer.concat(chunks);
  if (bytes.length > 64 * 1024) throw new Error('Owned fixture body too large');
  return bytes.length ? JSON.parse(bytes.toString()) : {};
}

function route(model, method, pathname, body, mode) {
  model.calls.push({ method, pathname, body });
  const ok = (data) => ({ success: true, data });
  const refused = (code) => ({ success: false, error: { code, message: `Owned refusal: ${code}` } });
  if (method === 'GET' && pathname === '/api/items/8/content') {
    const { calls, invokes, writebacks, feeSubmits, ...data } = model;
    return ok(structuredClone(data));
  }
  if (method === 'GET' && pathname === '/api/components/local') {
    return ok({ items: [{ id: 'owned-pack', name: 'Owned pack', enabled: true }], total: 1, total_pages: 1 });
  }
  if (method === 'GET' && pathname === '/api/components/local/owned-pack') {
    return ok({ id: 'owned-pack', template_json: { editable_params: [] } });
  }
  if (method === 'PUT' && pathname === '/api/items/8/translated') {
    if (model.manual_request_id || model.delivery_unresolved) return refused('REVIEW_DELIVERY_UNRESOLVED');
    const entries = body.content?.entries;
    if (!Array.isArray(entries) || entries.length !== 2
        || entries.map((entry) => entry.entry_id).sort().join(',') !== '101,102'
        || entries.some((entry) => typeof entry.msgstr !== 'string' || !entry.msgstr.includes('%s'))
        || Object.keys(body.content).join(',') !== 'entries') {
      return refused('REVIEW_ENTRY_SCOPE_CHANGED');
    }
    model.translated.payload.entries = structuredClone(entries);
    return ok({});
  }
  if (method === 'POST' && pathname === '/api/items/8/approve') {
    if (model.manual_request_id) return refused('REVIEW_REQUEST_UNRESOLVED');
    model.writebacks.push(structuredClone(model.translated.payload));
    if (mode === 'lost' && model.writebacks.length === 1) {
      model.delivery_unresolved = true;
      return refused('OWNED_LOST_ACK');
    }
    model.item.status = 'done';
    model.delivery_unresolved = false;
    return ok({ item_id: 8, status: 'done' });
  }
  if (method === 'PUT' && pathname === '/api/items/8/override') {
    if (model.manual_request_id || model.delivery_unresolved) return refused('REVIEW_DELIVERY_UNRESOLVED');
    return ok({ item_id: 8, component_id: 'owned-pack', source_lang: 'en', target_lang: 'zh', editable_overrides: {} });
  }
  if (method === 'POST' && pathname === '/api/items/8/retranslate') {
    if (model.delivery_unresolved) return refused('REVIEW_DELIVERY_UNRESOLVED');
    if (model.manual_request_id && (body.request_id !== request || body.resume_only !== true)) {
      return refused('MANUAL_REQUEST_UNKNOWN');
    }
    if (typeof body.request_id !== 'string') return refused('INVALID_REQUEST_ID');
    if (!body.resume_only) model.feeSubmits++;
    model.manual_request_id = null;
    model.translated.payload.entries = [
      { entry_id: 101, msgstr: 'Resumed button %s' }, { entry_id: 102, msgstr: 'Resumed menu %s' },
    ];
    return ok({ item_id: 8, request_id: body.request_id, status: 'pending_review' });
  }
  return refused('OWNED_ROUTE_NOT_IMPLEMENTED');
}

function invokeRoute(command, args) {
  const id = args.itemId;
  if (command === 'get_item_content') return ['GET', `/api/items/${id}/content`, {}];
  if (command === 'save_item_translated') return ['PUT', `/api/items/${id}/translated`, args.request];
  if (command === 'approve_item') return ['POST', `/api/items/${id}/approve`, {}];
  if (command === 'retranslate_item') return ['POST', `/api/items/${id}/retranslate`, args.request];
  if (command === 'proxy_webui_request') return [args.method, new URL(args.path, 'http://127.0.0.1').pathname, args.body ?? {}];
  throw new Error(`Owned unsupported Tauri command: ${command}`);
}

const entry = `
import { mount } from 'svelte';
import TranslationReview from '/src/pages/TranslationReview.svelte';
import Toast from '/src/lib/components/Toast.svelte';
import { setApiFetchHandler } from '/src/lib/api/client.ts';
import { locale } from 'svelte-i18n';
import '/src/app.css';
const params = new URLSearchParams(location.search);
const lang = params.get('lang') === 'zh-CN' ? 'zh-CN' : 'en';
if (params.get('client') === 'desktop') {
  await import('/@fs/${root}client-desktop/frontend/src/i18n/index.ts');
  const { apiFetch } = await import('/@fs/${root}client-desktop/frontend/src/lib/api/client.ts');
  window.__TAURI_INTERNALS__ = {
    invoke: async (command, args) => {
      const response = await fetch('/__owned_invoke', {
        method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({command,args}),
      });
      const result = await response.json();
      if (!result.success) throw result.error.code + ': ' + result.error.message;
      return result.data;
    },
  };
  setApiFetchHandler(apiFetch);
} else {
  await import('/src/i18n/index.ts');
}
locale.set(lang);
document.documentElement.lang = lang;
window.ownedErrors = [];
window.addEventListener('error', (event) => window.ownedErrors.push(String(event.message)));
window.addEventListener('unhandledrejection', (event) => window.ownedErrors.push(String(event.reason)));
mount(TranslationReview,{target:document.getElementById('review'),props:{itemId:8,onBack:()=>{}}});
mount(Toast,{target:document.getElementById('toasts')});
window.ownedReady = true;
`;

const port = await new Promise((resolve, reject) => {
  const reservation = createNetServer();
  reservation.once('error', reject);
  reservation.listen(0, '127.0.0.1', () => {
    const { port } = reservation.address();
    reservation.close(() => resolve(port));
  });
});
const server = await createServer({
  root: frontend, configFile: false, logLevel: 'warn',
  plugins: [
    tailwindcss(), svelte(),
    { name: 'owned-pack-entry',
      configureServer(server) { server.middlewares.use(fixtureMiddleware); },
      resolveId(id) { if (id === 'virtual:owned-pack-entry') return '\0owned-pack-entry'; },
      load(id) { if (id === '\0owned-pack-entry') return entry; },
    },
  ],
  resolve: { dedupe: ['svelte', 'svelte-i18n'] },
  server: { host: '127.0.0.1', port, strictPort: true, fs: { allow: [root] } },
});
async function fixtureMiddleware(req, res, next) {
  try {
    const pathname = new URL(req.url, 'http://127.0.0.1').pathname;
    if (pathname === '/') {
      res.setHeader('Content-Type', 'text/html');
      res.end('<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1"><title>Owned pack review</title></head><body><main id="review" style="height:100vh"></main><div id="toasts"></div><script type="module" src="/@id/__x00__owned-pack-entry"></script></body></html>');
      return;
    }
    if (pathname === '/__owned_evidence') {
      response(res, Object.fromEntries(models));
      return;
    }
    if (pathname === '/__owned_invoke' || pathname.startsWith('/api/')) {
      const { mode, model } = scope(req);
      const body = await readBody(req);
      if (pathname === '/__owned_invoke') model.invokes.push(structuredClone(body));
      const [method, target, payload] = pathname === '/__owned_invoke'
        ? invokeRoute(body.command, body.args)
        : [req.method, pathname, body];
      response(res, route(model, method, target, payload, mode));
      return;
    }
    next();
  } catch (error) {
    response(res, { success: false, error: { code: 'OWNED_FIXTURE_ERROR', message: String(error) } }, 500);
  }
}
await server.listen();
const address = server.httpServer.address();
fs.writeFileSync(output, JSON.stringify({
  pid: process.pid, base: `http://127.0.0.1:${address.port}`,
  fixture: fileURLToPath(import.meta.url), page: path.join(frontend, 'src/pages/TranslationReview.svelte'),
  limitation: 'Owned mock HTTP and Tauri browser adapter; native Rust bridge is tested separately.',
}, null, 2), { flag: 'wx' });
console.log(`Owned pack review ready at http://127.0.0.1:${address.port}`);
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, async () => { await server.close(); process.exit(0); });
}
