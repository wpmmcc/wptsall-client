#!/usr/bin/env bash
# Desktop pre-wp subset: Sites save+Test + Overview Run Once (shared WebUI testids).
#
# Requires: debug binary, tauri-driver, WebKitWebDriver, Xvfb, agent :8977,
# and PRE_WP_SITE_FIXTURE (or client-ui-setup/pre-wp-bind/fixtures/site.local.json).
#
#   bash tests/modules/client-desktop/tests/e2e/run-pre-wp-subset.sh
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
AGENT_BASE="${WPTSALL_DESKTOP_AGENT_BASE:-http://127.0.0.1:8977}"
FIXTURE="${PRE_WP_SITE_FIXTURE:-$ROOT_DIR/tests/modules/wpmmcc-ats/e2e/client-ui-setup/pre-wp-bind/fixtures/site.local.json}"
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

need_tool() { command -v "$1" >/dev/null 2>&1 || { echo "missing: $1" >&2; exit 2; }; }
need_tool npm
need_tool python3
need_tool xvfb-run
need_tool tauri-driver
[[ -n "$NATIVE_DRIVER" && -x "$NATIVE_DRIVER" ]] || { echo "missing WebKitWebDriver" >&2; exit 2; }
[[ -x "$APP_BIN" ]] || { echo "missing desktop binary: $APP_BIN" >&2; exit 2; }
[[ -f "$FIXTURE" ]] || { echo "missing site fixture: $FIXTURE (run export-site-fixture.sh)" >&2; exit 2; }
curl -fsS "${AGENT_BASE}/api/status" >/dev/null || { echo "agent missing at ${AGENT_BASE}" >&2; exit 2; }

setsid bash -lc "cd '$FRONTEND_DIR' && exec npm run dev -- --host 127.0.0.1 --port '$VITE_PORT'" >"$VITE_LOG" 2>&1 &
VITE_PID=$!
setsid env WPTSALL_DESKTOP_AGENT_BASE="$AGENT_BASE" xvfb-run -a tauri-driver \
  --port "$DRIVER_PORT" --native-port "$NATIVE_PORT" --native-driver "$NATIVE_DRIVER" \
  >"$DRIVER_LOG" 2>&1 &
DRIVER_PID=$!

python3 - "$APP_BIN" "$DRIVER_PORT" "$VITE_PORT" "$SESSION_FILE" "$FIXTURE" <<'PY'
import json, os, sys, time, urllib.request
from pathlib import Path

app, driver_port, vite_port, session_file, fixture_path = sys.argv[1:6]
base = f'http://127.0.0.1:{driver_port}'
site = json.loads(Path(fixture_path).read_text())

def wait_url(url, label, attempts=80):
    last = None
    for _ in range(attempts):
        try:
            urllib.request.urlopen(url, timeout=1).read(128)
            return
        except Exception as exc:
            last = exc
            time.sleep(0.25)
    raise SystemExit(f'{label} not ready: {last}')

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
  if (setter) setter.call(el, val); else el.value = val;
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
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

wait_url(f'http://127.0.0.1:{vite_port}/', 'vite')
wait_url(base + '/status', 'tauri-driver')
session = request('POST', '/session', {
    'capabilities': {'alwaysMatch': {
        'browserName': 'wry',
        'tauri:options': {'application': os.path.abspath(app), 'args': []},
    }},
})
sid = session.get('value', {}).get('sessionId') or session.get('sessionId')
if not sid:
    raise SystemExit(f'missing session: {session}')
open(session_file, 'w').write(sid)

for _ in range(80):
    text = js('return document.body && document.body.innerText || "";') or ''
    if 'WPTSALL' in text and 'Sites' in text:
        break
    time.sleep(0.5)
else:
    raise SystemExit('desktop UI missing')

js("""
const btn = Array.from(document.querySelectorAll('button')).find(b => (b.textContent||'').trim() === 'Sites');
if (btn) btn.click();
return !!btn;
""")
time.sleep(0.5)

saved = js(HELPERS + r'''
const site = arguments[0];
if (!set('sites-modal-url', site.api_base_url)) return { ok: false, error: 'url' };
if (!set('sites-modal-token', site.wp_client_token)) return { ok: false, error: 'token' };
if (!set('sites-modal-route-secret', site.route_secret || '')) return { ok: false, error: 'secret' };
const save = tid('sites-modal-save');
if (!save) return { ok: false, error: 'save missing' };
save.click();
return { ok: true };
''', [site])
if not saved or not saved.get('ok'):
    raise SystemExit(f'sites save failed: {saved}')

tested = poll(r'''
const btn = tid('sites-test-connection');
if (!btn) return { ok: false };
btn.click();
return { ok: true };
''', 15)
if not tested.get('ok'):
    raise SystemExit('sites-test-connection missing after save')

# Allow async test invoke to finish (do not busy-wait in sync JS)
time.sleep(2)
err = js(HELPERS + r'''
const err = document.querySelector('.text-red-700, .text-red-600');
const et = (err && err.textContent || '').trim();
if (et && /fail|error|route|token|unavailable/i.test(et)) {
  return { ok: false, fatal: true, error: et.slice(0, 240) };
}
return { ok: true };
''')
if err and err.get('fatal'):
    raise SystemExit(f'site test failed: {err.get("error")}')

js("""
const btn = Array.from(document.querySelectorAll('button')).find(b => (b.textContent||'').trim() === 'Overview');
if (btn) btn.click();
return !!btn;
""")
time.sleep(0.5)
run = js(HELPERS + r'''
const btn = tid('overview-run-once');
if (!btn) return { ok: false, error: 'overview-run-once missing' };
btn.click();
return { ok: true };
''')
if not run or not run.get('ok'):
    raise SystemExit(f'run once click failed: {run}')

# Poll until busy clears or summary appears (event-loop friendly)
done = poll(r'''
const btn = tid('overview-run-once');
const busy = btn && /Running/i.test(btn.textContent || '');
const body = (document.body && document.body.innerText) || '';
if (!busy && /Run once|processed|tasks_|complete|failed|succeeded/i.test(body)) {
  return { ok: true };
}
if (!busy && btn && !btn.disabled) return { ok: true };
return { ok: false };
''', 90)
if not done.get('ok'):
    body = js('return (document.body && document.body.innerText || "").slice(0, 500);')
    raise SystemExit(f'run once did not finish; body={body}')

print('desktop pre-wp subset passed (sites save+test + run-once)')
PY
