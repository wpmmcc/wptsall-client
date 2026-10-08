#!/usr/bin/env bash
# Desktop Tauri UI: fill provider wizard from mock lab-provider-cases (not hardcoded).
# Uses the same wizard-* / open-provider-wizard / provider-setup-wizard testids as WebUI
# (legacy desktop-* selectors remain as fallback).
#
# Requires: mock :9090 with GET /api/v1/lab-provider-cases, desktop debug binary,
# tauri-driver, WebKitWebDriver, Xvfb.
#
# Env:
#   LAB_CASES_LIMIT=5          # smoke subset (0 = all catalog:*)
#   LAB_CASE_FILTER=deepl,youdao
#   MOCK_API_BASE=http://127.0.0.1:9090
#
# Subset wrapper:
#   bash tests/modules/client-desktop/tests/e2e/run-lab-cases-subset.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../../../../lib/repo-root.sh
source "${SCRIPT_DIR}/../../../../lib/repo-root.sh"
ROOT_DIR="$(wptsall_repo_root "${SCRIPT_DIR}")"
FRONTEND_DIR="$ROOT_DIR/client-desktop/frontend"
APP_BIN="${WPTSALL_DESKTOP_APP:-$ROOT_DIR/client-desktop/src-tauri/target/debug/wptsall-desktop}"
DRIVER_PORT="${WPTSALL_TAURI_DRIVER_PORT:-4444}"
NATIVE_PORT="${WPTSALL_TAURI_NATIVE_PORT:-4445}"
VITE_PORT="${WPTSALL_DESKTOP_VITE_PORT:-1430}"
NATIVE_DRIVER="${WPTSALL_TAURI_NATIVE_DRIVER:-$(command -v WebKitWebDriver || true)}"
MOCK_API_BASE="${MOCK_API_BASE:-http://127.0.0.1:9090}"
REPORT_DIR="${REPORT_DIR:-$ROOT_DIR/tests/reports/e2e/client-desktop/ui-provider-mock}"
TMP_DIR="$(mktemp -d)"
VITE_LOG="$TMP_DIR/vite.log"
DRIVER_LOG="$TMP_DIR/tauri-driver.log"
SESSION_FILE="$TMP_DIR/session-id"
VITE_PID=""
DRIVER_PID=""

cleanup() {
  python3 - "$DRIVER_PORT" "$SESSION_FILE" >/dev/null 2>&1 <<'PY' || true
import sys, urllib.request
port, path = sys.argv[1], sys.argv[2]
try:
    sid = open(path).read().strip()
    req = urllib.request.Request(f'http://127.0.0.1:{port}/session/{sid}', method='DELETE')
    urllib.request.urlopen(req, timeout=2).read()
except Exception:
    pass
PY
  if [[ -n "${DRIVER_PID}" ]]; then
    kill -TERM -- "-$DRIVER_PID" >/dev/null 2>&1 || kill "$DRIVER_PID" >/dev/null 2>&1 || true
  fi
  if [[ -n "${VITE_PID}" ]]; then
    kill -TERM -- "-$VITE_PID" >/dev/null 2>&1 || kill "$VITE_PID" >/dev/null 2>&1 || true
  fi
  wait "$DRIVER_PID" "$VITE_PID" >/dev/null 2>&1 || true
  rm -rf "$TMP_DIR" 2>/dev/null || true
}
trap cleanup EXIT

need_tool() {
  command -v "$1" >/dev/null 2>&1 || { echo "missing: $1" >&2; exit 2; }
}
need_tool npm
need_tool python3
need_tool xvfb-run
need_tool tauri-driver
[[ -n "$NATIVE_DRIVER" && -x "$NATIVE_DRIVER" ]] || { echo "missing WebKitWebDriver" >&2; exit 2; }
[[ -x "$APP_BIN" ]] || { echo "missing desktop binary: $APP_BIN" >&2; exit 2; }

curl -fsS "${MOCK_API_BASE}/api/v1/lab-provider-cases" >/dev/null \
  || { echo "mock lab-provider-cases missing at ${MOCK_API_BASE}" >&2; exit 2; }

MOCK_API_BASE="${MOCK_API_BASE}" python3 "$ROOT_DIR/tests/scripts/sync-lab-provider-ui-cases.py"

AGENT_BASE="${WPTSALL_DESKTOP_AGENT_BASE:-http://127.0.0.1:8977}"
export WPTSALL_DESKTOP_AGENT_BASE="$AGENT_BASE"
curl -fsS "${AGENT_BASE}/api/status" >/dev/null \
  || { echo "desktop agent missing at ${AGENT_BASE} (start Client WebUI)" >&2; exit 2; }

setsid bash -lc "cd '$FRONTEND_DIR' && exec npm run dev -- --host 127.0.0.1 --port '$VITE_PORT'" >"$VITE_LOG" 2>&1 &
VITE_PID=$!
# Pass agent base into the app process launched by tauri-driver (inherits env).
setsid env \
  WPTSALL_DESKTOP_AGENT_BASE="$AGENT_BASE" \
  xvfb-run -a tauri-driver \
    --port "$DRIVER_PORT" \
    --native-port "$NATIVE_PORT" \
    --native-driver "$NATIVE_DRIVER" \
    >"$DRIVER_LOG" 2>&1 &
DRIVER_PID=$!

mkdir -p "$REPORT_DIR"
export MOCK_API_BASE REPORT_DIR
export LAB_CASES_LIMIT="${LAB_CASES_LIMIT:-0}"
export LAB_CASE_FILTER="${LAB_CASE_FILTER:-}"
export LAB_INCLUDE_AUTH_PROFILES="${LAB_INCLUDE_AUTH_PROFILES:-0}"

python3 - "$APP_BIN" "$DRIVER_PORT" "$VITE_PORT" "$SESSION_FILE" <<'PY'
import json, os, sys, time, urllib.error, urllib.request
from pathlib import Path

app, driver_port, vite_port, session_file = sys.argv[1:5]
base = f'http://127.0.0.1:{driver_port}'
mock = os.environ.get('MOCK_API_BASE', 'http://127.0.0.1:9090').rstrip('/')
report_dir = Path(os.environ.get('REPORT_DIR', '.'))
limit = int(os.environ.get('LAB_CASES_LIMIT') or '0')
filt = [s.strip() for s in (os.environ.get('LAB_CASE_FILTER') or '').split(',') if s.strip()]
include_profiles = os.environ.get('LAB_INCLUDE_AUTH_PROFILES') == '1'

def wait_url(url, label, attempts=80):
    last = None
    for _ in range(attempts):
        try:
            urllib.request.urlopen(url, timeout=1).read(128)
            return
        except Exception as exc:
            last = exc
            time.sleep(0.25)
    print(f'{label} not ready: {last}', file=sys.stderr)
    sys.exit(3)

def request(method, path, payload=None, timeout=40):
    data = None if payload is None else json.dumps(payload).encode()
    req = urllib.request.Request(
        base + path, data=data, method=method,
        headers={'Content-Type': 'application/json'},
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode() or '{}')

def js(script, args=None, timeout=30):
    return request(
        'POST', f'/session/{sid}/execute/sync',
        {'script': script, 'args': args or []}, timeout=timeout,
    ).get('value')

wait_url(f'http://127.0.0.1:{vite_port}/', 'vite')
wait_url(base + '/status', 'tauri-driver')
with urllib.request.urlopen(f'{mock}/api/v1/lab-provider-cases', timeout=30) as resp:
    inv = json.loads(resp.read().decode())
cases = inv.get('cases') or []
cases = [c for c in cases if str(c.get('case_id', '')).startswith('catalog:') or include_profiles]
if not include_profiles:
    cases = [c for c in cases if str(c.get('case_id', '')).startswith('catalog:')]
if filt:
    cases = [c for c in cases if any(f in c.get('case_id', '') or f in c.get('entry_id', '') for f in filt)]
if limit > 0:
    cases = cases[:limit]
if not cases:
    print('no lab cases selected', file=sys.stderr)
    sys.exit(2)

session = request('POST', '/session', {
    'capabilities': {'alwaysMatch': {
        'browserName': 'wry',
        'tauri:options': {
            'application': os.path.abspath(app),
            'args': [],
        },
    }},
})
sid = session.get('value', {}).get('sessionId') or session.get('sessionId')
if not sid:
    print(f'missing session: {session}', file=sys.stderr)
    sys.exit(5)
open(session_file, 'w').write(sid)

for _ in range(80):
    text = js('return document.body && document.body.innerText || "";') or ''
    if 'WPTSALL' in text and ('API Keys' in text or 'API 密钥' in text):
        break
    time.sleep(0.5)
else:
    print('desktop UI missing', file=sys.stderr)
    sys.exit(6)

js("""
const btn = Array.from(document.querySelectorAll('button')).find(b => {
  const t = (b.textContent||'').trim();
  return t === 'API Keys' || t === 'API 密钥';
});
if (btn) btn.click();
return !!btn;
""")
time.sleep(0.8)
# Load catalog and wait until shared (or legacy) open-wizard buttons appear.
js("""
const btns = Array.from(document.querySelectorAll('button'));
const reload = btns.find(b => /Reload|Loading/i.test(b.textContent||''));
if (reload && !reload.disabled) reload.click();
return true;
""")
open_count = 0
for _ in range(60):
    open_count = int(js("""
return document.querySelectorAll(
  '[data-testid="open-provider-wizard"],[data-testid^="desktop-open-wizard-"]'
).length || 0;
""") or 0)
    if open_count > 0:
        break
    time.sleep(0.5)
if open_count <= 0:
    diag = js("""
const ids = Array.from(document.querySelectorAll('[data-testid]'))
  .map(el => el.getAttribute('data-testid')).filter(Boolean).slice(0, 40);
const entries = Array.from(document.querySelectorAll('[data-entry-id]'))
  .map(el => el.getAttribute('data-entry-id')).slice(0, 20);
const body = (document.body && document.body.innerText || '').slice(0, 800);
return { ids, entries, body };
""") or {}
    print('catalog open buttons missing after reload', file=sys.stderr)
    print(json.dumps(diag, indent=2)[:2000], file=sys.stderr)
    sys.exit(6)
print(f'catalog open buttons: {open_count}')

# IMPORTANT: never busy-wait inside execute/sync — it blocks the JS event loop and
# async Tauri invoke (install / key / quick-test) never completes. Poll from Python.
HELPERS = r'''
const tid = (...ids) => {
  for (const id of ids) {
    const el = document.querySelector(`[data-testid="${id}"]`);
    if (el) return el;
  }
  return null;
};
const set = (ids, val) => {
  const el = tid(...(Array.isArray(ids) ? ids : [ids]));
  if (!el) return false;
  el.focus();
  const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value')?.set;
  if (setter) setter.call(el, val);
  else el.value = val;
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
  return true;
};
const click = (ids) => {
  const el = tid(...(Array.isArray(ids) ? ids : [ids]));
  if (!el) return false;
  el.click();
  return true;
};
'''

def poll(script, timeout_s, interval=0.4):
    deadline = time.time() + timeout_s
    last = None
    while time.time() < deadline:
        last = js(HELPERS + script)
        if last and last.get('ok'):
            return last
        if last and last.get('fatal'):
            return last
        time.sleep(interval)
    return last or {'ok': False, 'error': 'poll timeout'}

def run_case(c):
    stamp = format(int(time.time() * 1000), 'x')[-8:]
    entry = c.get('entry_id')
    start = js(HELPERS + r'''
const c = arguments[0], stamp = arguments[1], entry = arguments[2];
const openBtn =
  document.querySelector(`[data-testid="open-provider-wizard"][data-entry-id="${entry}"]`) ||
  document.querySelector(`[data-testid="desktop-open-wizard-${entry}"]`);
if (!openBtn) return { ok: false, error: 'open button missing for ' + entry };
openBtn.scrollIntoView({ block: 'center' });
openBtn.click();
if (!tid('provider-setup-wizard', 'desktop-provider-wizard')) {
  return { ok: false, error: 'wizard not open' };
}
if (!set(['wizard-local-id', 'desktop-wizard-local-id'], (`lab-${entry}-${stamp}`).slice(0, 100))) {
  return { ok: false, error: 'local-id' };
}
if (!click(['wizard-install-next', 'desktop-wizard-install'])) return { ok: false, error: 'install click' };
return { ok: true };
''', [c, stamp, entry])
    if not start or not start.get('ok'):
        return start or {'ok': False, 'error': 'start failed'}

    key_ready = poll(r'''
if (tid('wizard-api-key', 'desktop-wizard-api-key')) return { ok: true };
const err = document.querySelector('.text-red-700, .text-red-600, [data-testid="wizard-hint"]');
const et = (err && err.textContent || '').trim();
if (et && /fail|error|not found|unavailable|INVOKE/i.test(et)) {
  return { ok: false, fatal: true, error: 'install: ' + et.slice(0, 240) };
}
return { ok: false };
''', 45)
    if not key_ready.get('ok'):
        body = js('return (document.body && document.body.innerText || "").slice(0, 600);') or ''
        return {
            'ok': False,
            'error': key_ready.get('error') or ('key step missing; body=' + str(body)[:600]),
        }

    keyed = js(HELPERS + r'''
const c = arguments[0], stamp = arguments[1], entry = arguments[2];
set(['wizard-key-id', 'desktop-wizard-key-id'], (`lab-${entry}-k-${stamp}`).slice(0, 100));
const fields = c.auth_fields && c.auth_fields.length ? c.auth_fields : Object.keys(c.auth_values || {});
const vals = c.auth_values || {};
fields.forEach((name, i) => {
  const v = vals[name];
  if (v == null || v === '') return;
  const shared = i === 0 ? 'wizard-api-key' : `wizard-auth-${name}`;
  const legacy = i === 0 ? 'desktop-wizard-api-key' : `desktop-wizard-auth-${name}`;
  set([shared, legacy], String(v));
});
set(['wizard-request-url', 'desktop-wizard-request-url'], c.request_url || '');
if (c.response_translated_text_path) {
  set(['wizard-response-path', 'desktop-wizard-response-path'], c.response_translated_text_path);
}
if (c.family === 'openai_compatible') {
  set(['wizard-model', 'desktop-wizard-model'], 'mock-openai-v1');
}
if (!click(['wizard-key-next', 'desktop-wizard-key-next'])) return { ok: false, error: 'key-next' };
return { ok: true };
''', [c, stamp, entry])
    if not keyed or not keyed.get('ok'):
        return keyed or {'ok': False, 'error': 'key fill failed'}

    test_ready = poll(r'''
if (tid('wizard-test-next', 'desktop-wizard-test-next')) return { ok: true };
const err = document.querySelector('.text-red-700, .text-red-600');
const et = (err && err.textContent || '').trim();
if (et && /fail|error|INVOKE/i.test(et)) {
  return { ok: false, fatal: true, error: 'key-bind: ' + et.slice(0, 240) };
}
return { ok: false };
''', 25)
    if not test_ready.get('ok'):
        return {'ok': False, 'error': test_ready.get('error') or 'test step missing'}

    tested = js(HELPERS + r'''
const c = arguments[0], entry = arguments[1];
set(['wizard-test-text', 'desktop-wizard-test-text'], 'Hello world');
// Provider-native test languages, mirroring the WebUI spec: the case
// carries the pair the mock expects (e.g. baidu requires zh, rejects
// zh_CN); DeepL uses uppercase EN/ZH.
const isDeepL = /deepl/i.test(String(c.entry_id || '') + String(entry || ''));
const src = isDeepL ? 'EN' : (c.source_lang || (c.fill && c.fill.source_lang) || 'en');
const tgt = isDeepL ? 'ZH' : (c.target_lang || (c.fill && c.fill.target_lang) || 'zh');
if (!set(['wizard-test-source-lang'], src)) return { ok: false, error: 'source-lang input' };
if (!set(['wizard-test-target-lang'], tgt)) return { ok: false, error: 'target-lang input' };
if (!click(['wizard-test-next', 'desktop-wizard-test-next'])) return { ok: false, error: 'test-next' };
return { ok: true };
''', [c, entry])
    if not tested or not tested.get('ok'):
        return tested or {'ok': False, 'error': 'test click failed'}

    enable_ready = poll(r'''
if (tid('wizard-enable-next', 'desktop-wizard-enable')) return { ok: true };
const errRoot = tid('provider-setup-wizard', 'desktop-provider-wizard');
const err = errRoot && errRoot.querySelector('.text-red-700, .text-red-600');
const et = (err && err.textContent || '').trim();
if (et) return { ok: false, fatal: true, error: 'quick-test: ' + et.slice(0, 240) };
const g = document.querySelector('.text-red-700');
const gt = (g && g.textContent || '').trim();
if (gt && /fail|error|INVOKE|unavailable/i.test(gt)) {
  return { ok: false, fatal: true, error: 'quick-test: ' + gt.slice(0, 240) };
}
return { ok: false };
''', 60)
    if not enable_ready.get('ok'):
        body = js('return (document.body && document.body.innerText || "").slice(0, 500);') or ''
        return {
            'ok': False,
            'error': enable_ready.get('error') or ('enable step missing; body=' + str(body)[:500]),
        }

    js(HELPERS + "click(['wizard-enable-next', 'desktop-wizard-enable']); return true;")
    route_ready = poll(
        "return { ok: !!tid('wizard-route-next', 'desktop-wizard-route') };", 20
    )
    if not route_ready.get('ok'):
        return {'ok': False, 'error': 'route step missing'}
    js(HELPERS + "click(['wizard-route-next', 'desktop-wizard-route']); return true;")
    done = poll("return { ok: !!tid('wizard-done', 'desktop-wizard-done') };", 20)
    if not done.get('ok'):
        return {'ok': False, 'error': 'done missing'}
    js(HELPERS + r'''
const close =
  tid('wizard-close') ||
  Array.from((tid('provider-setup-wizard', 'desktop-provider-wizard') || document).querySelectorAll('button'))
    .find(b => /Close/i.test(b.textContent || ''));
if (close) close.click();
return true;
''')
    return {'ok': True}

results = []
for c in cases:
    try:
        out = run_case(c)
        ok = bool(out and out.get('ok'))
        results.append({'case_id': c.get('case_id'), 'ok': ok, 'error': None if ok else (out or {}).get('error')})
        print(('PASS' if ok else 'FAIL'), c.get('case_id'), '' if ok else (out or {}).get('error'))
    except Exception as exc:
        results.append({'case_id': c.get('case_id'), 'ok': False, 'error': str(exc)})
        print('FAIL', c.get('case_id'), exc)

failed = [r for r in results if not r['ok']]
stamp = time.strftime('%Y%m%dT%H%M%S')
report = {
    'task': 'ui-desktop-lab-provider-cases',
    'source': f'{mock}/api/v1/lab-provider-cases',
    'total': len(results),
    'passed': len(results) - len(failed),
    'failed': len(failed),
    'results': results,
}
out_path = report_dir / f'desktop-lab-cases-{stamp}.json'
out_path.write_text(json.dumps(report, indent=2) + '\n')
print('report:', out_path)
if failed:
    print(json.dumps(failed, indent=2), file=sys.stderr)
    sys.exit(1)
print('desktop lab-provider-cases UI passed', len(results))
PY
