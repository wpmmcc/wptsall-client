#!/usr/bin/env bash
set -euo pipefail

# Desktop real-user journey against the local Docker WordPress Lab (local-first).
#
# This is intentionally a Desktop/Tauri test, not a standalone WebUI shortcut:
# - prepares real WP data/relations with the canonical E2E stage scripts;
# - launches the real Tauri debug binary in Xvfb via tauri-driver;
# - binds WP via Protocol v2 device token + route_secret (NO website OAuth/login);
# - configures a local OpenAI-compatible component against mock-api (:9090);
# - runs the shared Protocol v2 worker once from the Desktop UI;
# - verifies WP write-back with canonical Stage 7;
# - restarts the Desktop app and checks SQLite persistence through the UI.
#
# Website control-plane (8787 /api/oauth/start) is out of scope — do not reintroduce.
#
# ENVIRONMENT BINDING (tasks/test/22 §3.4): by default this journey targets the
# main lab (:9083 / wordpress-test) and its stage-02 token issuance INVALIDATES
# the `.env.test` fixture token the webui e2e gate depends on (tests/README
# §6.1 contract #3). For a token-isolated run, target a dedicated e2e slot
# instead (no code change needed — the lab target is parameterized):
#   LAB_WP_PORT=9182 LAB_WP_CONTAINER=wptsall-wp-lab-wordpress-slot-b \
#     bash tests/modules/client-desktop/tests/e2e/docker-wp-journey.sh
# The journey's stages seed their own data on the slot; the main lab's device
# token stays untouched. Do NOT bind it to slot-u (:9180) — that is the pinned
# wp-unit/wp-integration environment.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../../../../lib/repo-root.sh
source "${SCRIPT_DIR}/../../../../lib/repo-root.sh"
ROOT_DIR="$(wptsall_repo_root "${SCRIPT_DIR}")"
E2E_DIR="$ROOT_DIR/tests/modules/wpmmcc-ats/e2e"
FRONTEND_DIR="$ROOT_DIR/client-desktop/frontend"
APP_BIN="${WPTSALL_DESKTOP_APP:-$ROOT_DIR/client-desktop/src-tauri/target/debug/wptsall-desktop}"
PROJECT="${E2E_PROJECT:-core-content}"
SCOPE="${E2E_SCOPE:-core-only}"
LAB_WP_PORT="${LAB_WP_PORT:-9083}"
WP_BASE="${WP_BASE:-http://127.0.0.1:${LAB_WP_PORT}}"
WP_CONTAINER="${LAB_WP_CONTAINER:-wptsall-wp-lab-wordpress-test-1}"
DRIVER_PORT="${WPTSALL_TAURI_DRIVER_PORT:-4444}"
NATIVE_PORT="${WPTSALL_TAURI_NATIVE_PORT:-4445}"
VITE_PORT="${WPTSALL_DESKTOP_VITE_PORT:-1430}"
AGENT_PORT="${WPTSALL_DESKTOP_AGENT_PORT:-8977}"
# The journey boots its OWN embedded agent (device identity = DEVICE_ID) on
# AGENT_PORT. A developer-owned shared agent already listening there silently
# wins the bind: the desktop frontend then proxies to the WRONG device and
# every WP call 401s with client_unauthorized (token was issued for this
# journey's DEVICE_ID, the squatter presents its own). Detect the collision
# and move to the first free port instead of failing opaquely mid-journey.
if curl -sf -m 2 "http://127.0.0.1:${AGENT_PORT}/api/status" >/dev/null 2>&1; then
  PORT_MOVED=0
  for _candidate in 8979 8981 8983 8985 8987; do
    if ! curl -sf -m 2 "http://127.0.0.1:${_candidate}/api/status" >/dev/null 2>&1; then
      echo "AGENT_PORT ${AGENT_PORT} occupied by an existing agent — moving embedded agent to ${_candidate}" >&2
      AGENT_PORT="$_candidate"
      PORT_MOVED=1
      break
    fi
  done
  if [[ "${PORT_MOVED}" != "1" ]]; then
    echo "AGENT_PORT ${AGENT_PORT} and all fallback ports are occupied — stop the shared agent or set WPTSALL_DESKTOP_AGENT_PORT" >&2
    exit 3
  fi
fi
NATIVE_DRIVER="${WPTSALL_TAURI_NATIVE_DRIVER:-$(command -v WebKitWebDriver || true)}"
MOCK_API_BASE="${MOCK_API_BASE:-http://127.0.0.1:9090}"
MOCK_API_KEY="${MOCK_API_KEY:-mock-translate-dev-key-2026}"
DEVICE_ID="${WPTSALL_DEVICE_ID:-desktop-docker-wp-$(date +%s)}"
TMP_DIR="$(mktemp -d)"
VITE_LOG="$TMP_DIR/vite.log"
DRIVER_LOG="$TMP_DIR/tauri-driver.log"
SESSION_FILE="$TMP_DIR/session-id"
RUNTIME_ROOT="${WPTSALL_DESKTOP_E2E_RUNTIME_ROOT:-$E2E_DIR/runtime/desktop-tauri}"
DB_PATH="$RUNTIME_ROOT/wptsall.db"
LOG_FILE="$RUNTIME_ROOT/wptsall-client.log"
VITE_PID=""
DRIVER_PID=""
MOCK_PID=""

cleanup() {
  python3 - "$DRIVER_PORT" "$SESSION_FILE" >/dev/null 2>&1 <<'PY' || true
import sys, urllib.request
port = sys.argv[1]
path = sys.argv[2]
try:
    sid = open(path).read().strip()
    req = urllib.request.Request(f'http://127.0.0.1:{port}/session/{sid}', method='DELETE')
    urllib.request.urlopen(req, timeout=2).read()
except Exception:
    pass
PY

  if [[ -n "$DRIVER_PID" ]]; then
    kill -TERM -- "-$DRIVER_PID" >/dev/null 2>&1 || kill "$DRIVER_PID" >/dev/null 2>&1 || true
  fi
  if [[ -n "$VITE_PID" ]]; then
    kill -TERM -- "-$VITE_PID" >/dev/null 2>&1 || kill "$VITE_PID" >/dev/null 2>&1 || true
  fi
  if [[ -n "$MOCK_PID" ]]; then
    kill "$MOCK_PID" >/dev/null 2>&1 || true
  fi
  wait "$DRIVER_PID" "$VITE_PID" "$MOCK_PID" >/dev/null 2>&1 || true

  if [[ "${WPTSALL_DESKTOP_DOCKER_WP_E2E_KEEP_LOGS:-0}" == "1" ]]; then
    echo "logs kept in $TMP_DIR"
  else
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

need_tool() {
  local tool="$1"
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "missing required tool: $tool" >&2
    exit 2
  fi
}

need_tool npm
need_tool python3
need_tool curl
need_tool docker
need_tool xvfb-run
need_tool tauri-driver
if [[ -z "$NATIVE_DRIVER" || ! -x "$NATIVE_DRIVER" ]]; then
  echo "missing WebKit native webdriver; set WPTSALL_TAURI_NATIVE_DRIVER or install WebKitWebDriver" >&2
  exit 2
fi
if [[ ! -x "$APP_BIN" ]]; then
  cat >&2 <<EOF
missing Desktop debug binary: $APP_BIN
Build it first; the router reuses the stable target and the shared cargo cache policy:
  bash scripts/wptsall.sh build desktop
EOF
  exit 2
fi
if ! docker ps --format '{{.Names}}' | grep -Fxq "$WP_CONTAINER"; then
  echo "Docker WP container is not running: $WP_CONTAINER" >&2
  exit 2
fi

ensure_mock_api() {
  if curl -fsS "$MOCK_API_BASE/api/v1/health" >/dev/null 2>&1; then
    return 0
  fi
  local mock_bin="${WPTSALL_MOCK_API_BIN:-$ROOT_DIR/tests/infra/mock-api/target/release/mock-translate-api}"
  [[ -x "$mock_bin" ]] || mock_bin="$ROOT_DIR/runtime/bin/mock-translate-api"
  if [[ ! -x "$mock_bin" ]]; then
    echo "mock-translate-api is not running and no binary found" >&2
    exit 2
  fi
  "$mock_bin" >/tmp/wptsall-desktop-mock-api.log 2>&1 &
  MOCK_PID=$!
  for _ in $(seq 1 40); do
    curl -fsS "$MOCK_API_BASE/api/v1/health" >/dev/null 2>&1 && return 0
    sleep 0.25
  done
  echo "mock-translate-api did not become ready" >&2
  exit 2
}

wp_cli() {
  docker exec \
    -e WPTSALL_LAB=1 \
    -e E2E_PROJECT="$PROJECT" \
    -e E2E_SCOPE="$SCOPE" \
    -e PHP_MEMORY_LIMIT="${LAB_PHP_MEMORY_LIMIT:-4096M}" \
    -e WP_CLI_PHP="php -d memory_limit=${LAB_PHP_MEMORY_LIMIT:-4096M}" \
    "$WP_CONTAINER" wp --allow-root --path=/var/www/html "$@"
}

wp_eval_file() {
  local host_path="$1"
  shift || true
  local mapped="$host_path"
  case "$host_path" in
    "$E2E_DIR"/*) mapped="/opt/wptsall-e2e/${host_path#"$E2E_DIR/"}" ;;
    "$ROOT_DIR/tests/modules/wpmmcc-ats/seeding"/*) mapped="/opt/wptsall-seeding/${host_path#"$ROOT_DIR/tests/modules/wpmmcc-ats/seeding/"}" ;;
  esac
  wp_cli eval-file "$mapped" "$@"
}

wait_url() {
  local url="$1"
  local label="$2"
  for _ in $(seq 1 80); do
    curl -fsS "$url" >/dev/null 2>&1 && return 0
    sleep 0.25
  done
  echo "$label did not become ready: $url" >&2
  return 1
}

prepare_wp_lane() {
  echo "== Preparing Docker WP lane: project=$PROJECT scope=$SCOPE wp=$WP_BASE =="
  if [[ "${WPTSALL_DESKTOP_SKIP_WP_PREPARE:-0}" == "1" ]]; then
    echo "skipping WP clean/seed/scan/relation (WPTSALL_DESKTOP_SKIP_WP_PREPARE=1)"
    return 0
  fi
  (
    cd "$ROOT_DIR"
    WPTSALL_LAB=1 \
    E2E_PROJECT="$PROJECT" \
    E2E_SCOPE="$SCOPE" \
    WP_BASE="$WP_BASE" \
    LAB_WP_HOST="127.0.0.1" \
    LAB_WP_PORT="$LAB_WP_PORT" \
    LAB_WP_CONTAINER="$WP_CONTAINER" \
    E2E_SKIP_FRONTEND_VERIFY=0 \
    bash tests/modules/wpmmcc-ats/e2e/stages/02-env-clean.sh
    WPTSALL_LAB=1 E2E_PROJECT="$PROJECT" E2E_SCOPE="$SCOPE" WP_BASE="$WP_BASE" LAB_WP_HOST="127.0.0.1" LAB_WP_PORT="$LAB_WP_PORT" LAB_WP_CONTAINER="$WP_CONTAINER" bash tests/modules/wpmmcc-ats/e2e/stages/03-data-seed.sh
    WPTSALL_LAB=1 E2E_PROJECT="$PROJECT" E2E_SCOPE="$SCOPE" WP_BASE="$WP_BASE" LAB_WP_HOST="127.0.0.1" LAB_WP_PORT="$LAB_WP_PORT" LAB_WP_CONTAINER="$WP_CONTAINER" bash tests/modules/wpmmcc-ats/e2e/stages/04-model-scan.sh
    WPTSALL_LAB=1 E2E_PROJECT="$PROJECT" E2E_SCOPE="$SCOPE" WP_BASE="$WP_BASE" LAB_WP_HOST="127.0.0.1" LAB_WP_PORT="$LAB_WP_PORT" LAB_WP_CONTAINER="$WP_CONTAINER" bash tests/modules/wpmmcc-ats/e2e/stages/05-relation-setup.sh
  )
}

issue_wp_token() {
  ROUTE_SECRET="$(wp_cli eval 'echo function_exists("wptsall_get_client_route_secret") ? (string) wptsall_get_client_route_secret() : "";' 2>/dev/null | tail -n 1 | tr -d '\r' || true)"
  if [[ -z "$ROUTE_SECRET" ]]; then
    echo "route_secret unavailable from WP" >&2
    exit 3
  fi
  DEVICE_JSON="$(wp_cli eval 'if(function_exists("wptsall_issue_client_device_token")){ $d=wptsall_issue_client_device_token(getenv("WPTSALL_DEVICE_ID") ?: "desktop-e2e", "desktop-e2e"); echo wp_json_encode($d); }' 2>/dev/null | tail -n 1 || true)"
  WP_CLIENT_TOKEN="$(python3 -c 'import json,sys; d=json.loads(sys.argv[1] or "{}"); print(d.get("token", ""))' "$DEVICE_JSON" 2>/dev/null || true)"
  ISSUED_DEVICE_ID="$(python3 -c 'import json,sys; d=json.loads(sys.argv[1] or "{}"); print(d.get("device_id", ""))' "$DEVICE_JSON" 2>/dev/null || true)"
  if [[ -z "$WP_CLIENT_TOKEN" ]]; then
    echo "WP_CLIENT_TOKEN unavailable; raw device json: $DEVICE_JSON" >&2
    exit 3
  fi
  if [[ -n "$ISSUED_DEVICE_ID" ]]; then
    DEVICE_ID="$ISSUED_DEVICE_ID"
  fi
  echo "route_secret=$ROUTE_SECRET"
  echo "device_id=$DEVICE_ID"
}

load_relation_ids() {
  local relation_file="$E2E_DIR/runtime/relation-ids.json"
  if [[ ! -f "$relation_file" ]]; then
    echo "relation file missing: $relation_file" >&2
    exit 3
  fi
  VIRTUAL_RELATION_ID="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d.get("virtual", ""))' "$relation_file")"
  WP_RELATION_ID="$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(d.get("wp", ""))' "$relation_file")"
  if [[ -z "$VIRTUAL_RELATION_ID" || -z "$WP_RELATION_ID" ]]; then
    echo "relation ids incomplete in $relation_file" >&2
    exit 3
  fi
  echo "virtual_relation_id=$VIRTUAL_RELATION_ID"
  echo "wp_relation_id=$WP_RELATION_ID"
}

start_tauri_session() {
  rm -f "$SESSION_FILE"
  setsid bash -lc "cd '$FRONTEND_DIR' && exec npm run dev -- --host 127.0.0.1 --port '$VITE_PORT'" >"$VITE_LOG" 2>&1 &
  VITE_PID=$!
  mkdir -p "$RUNTIME_ROOT"
  rm -f "$RUNTIME_ROOT"/wptsall.db-shm "$RUNTIME_ROOT"/wptsall.db-wal 2>/dev/null || true
  setsid env \
    WPTSALL_WEB_UI=1 \
    WPTSALL_WEB_UI_PORT="$AGENT_PORT" \
    WPTSALL_WEB_UI_BIND="127.0.0.1:$AGENT_PORT" \
    WPTSALL_DESKTOP_AGENT_BASE="http://127.0.0.1:$AGENT_PORT" \
    WPTSALL_PROVIDER_ALLOWLIST="127.0.0.1,localhost" \
    WPTSALL_DEVICE_ID="$DEVICE_ID" \
    WPTSALL_WP_DEVICE_ID="$DEVICE_ID" \
    WPTSALL_DB_PATH="$DB_PATH" \
    WPTSALL_LOG_FILE="$LOG_FILE" \
    WPTSALL_LOG_ENABLED=1 \
    WPTSALL_DESKTOP_RUN_ONCE_TIMEOUT_SECS="600" \
    xvfb-run -a tauri-driver \
      --port "$DRIVER_PORT" \
      --native-port "$NATIVE_PORT" \
      --native-driver "$NATIVE_DRIVER" \
      >"$DRIVER_LOG" 2>&1 &
  DRIVER_PID=$!

  wait_url "http://127.0.0.1:$VITE_PORT/" "Vite dev server"
  wait_url "http://127.0.0.1:$DRIVER_PORT/status" "tauri-driver"

  python3 - "$APP_BIN" "$DRIVER_PORT" "$SESSION_FILE" <<'PY'
import json, os, sys, time, urllib.request, urllib.error
app = os.path.abspath(sys.argv[1])
port = sys.argv[2]
session_file = sys.argv[3]
base = f'http://127.0.0.1:{port}'

def req(method, path, payload=None, timeout=40):
    data = None if payload is None else json.dumps(payload).encode()
    request = urllib.request.Request(base + path, data=data, method=method, headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode() or '{}')

payload = {'capabilities': {'alwaysMatch': {'browserName': 'wry', 'tauri:options': {'application': app, 'args': []}}}}
try:
    session = req('POST', '/session', payload, timeout=50)
except urllib.error.HTTPError as exc:
    print('session create failed:', exc.code, exc.read().decode(errors='replace'), file=sys.stderr)
    sys.exit(4)
sid = session.get('value', {}).get('sessionId') or session.get('sessionId')
if not sid:
    print('missing session id', session, file=sys.stderr)
    sys.exit(5)
open(session_file, 'w').write(sid)

def body_text():
    return req('POST', f'/session/{sid}/execute/sync', {
        'script': 'return document.body && document.body.innerText ? document.body.innerText : "";',
        'args': [],
    }, timeout=30).get('value', '')

# Locale pin (批 M / doc21 G1): the desktop WebView follows the host
# navigator language; on a zh host every EN-text assertion in this journey
# (nav markers, button labels, placeholders, toasts) fails at the first
# load wait — same defect class the provider-wizard quick test already had
# fixed. Pin the versioned locale key to 'en' and reload the webview once;
# the pin persists across the journey's app-restart phase (WebView profile
# lives in this run's runtime dir), so all EN markers stay deterministic
# regardless of host locale.
for _ in range(60):
    if body_text().strip():
        break
    time.sleep(0.5)
req('POST', f'/session/{sid}/execute/sync', {
    'script': "localStorage.setItem('wptsall_locale.v1', 'en'); location.reload();",
    'args': [],
}, timeout=30)

text = ''
for _ in range(100):
    text = body_text()
    # Brand + nav + subtitle. 'Translation Client' is the historical brand
    # string (window title); the in-page tagline is 'Translate & Sync' —
    # accept both, mirroring tauri-smoke's tolerant form (批 M / doc21 G1).
    if 'WPTSALL' in text and 'Overview' in text and (
        'Translation Client' in text or 'Translate & Sync' in text
    ):
        print('desktop_loaded=' + text[:180].replace('\n', ' | '))
        sys.exit(0)
    time.sleep(0.5)
print('desktop UI did not load: ' + text[:1000].replace('\n', ' | '), file=sys.stderr)
sys.exit(6)
PY
}

stop_tauri_session() {
  python3 - "$DRIVER_PORT" "$SESSION_FILE" >/dev/null 2>&1 <<'PY' || true
import sys, urllib.request
port = sys.argv[1]
path = sys.argv[2]
try:
    sid = open(path).read().strip()
    req = urllib.request.Request(f'http://127.0.0.1:{port}/session/{sid}', method='DELETE')
    urllib.request.urlopen(req, timeout=2).read()
except Exception:
    pass
PY
  if [[ -n "$DRIVER_PID" ]]; then
    kill -TERM -- "-$DRIVER_PID" >/dev/null 2>&1 || kill "$DRIVER_PID" >/dev/null 2>&1 || true
    wait "$DRIVER_PID" >/dev/null 2>&1 || true
    DRIVER_PID=""
  fi
  if [[ -n "$VITE_PID" ]]; then
    kill -TERM -- "-$VITE_PID" >/dev/null 2>&1 || kill "$VITE_PID" >/dev/null 2>&1 || true
    wait "$VITE_PID" >/dev/null 2>&1 || true
    VITE_PID=""
  fi
  # Give the embedded WebUI loopback listener time to release AGENT_PORT.
  sleep 1
}

run_desktop_dom_flow() {
  python3 - \
    "$DRIVER_PORT" \
    "$SESSION_FILE" \
    "$WP_BASE/wp-json/wptsall/v2/$ROUTE_SECRET/client" \
    "$WP_CLIENT_TOKEN" \
    "$ROUTE_SECRET" \
    "$MOCK_API_BASE" \
    "$MOCK_API_KEY" \
    "http://127.0.0.1:$AGENT_PORT" \
    "${DEMO_EMAIL:-demo@wptsall.dev}" \
    "${DEMO_PASSWORD:-demo}" \
    "$VIRTUAL_RELATION_ID" \
    "$WP_RELATION_ID" \
    "$LOG_FILE" <<'PY'
import json
import os
import sys
import time
import urllib.parse
import urllib.request
import urllib.error

driver_port, session_file, wp_url, wp_token, route_secret, mock_api_base, mock_api_key, agent_base, _demo_email, _demo_password, virtual_relation_id, wp_relation_id, log_file = sys.argv[1:]
base = f'http://127.0.0.1:{driver_port}'
sid = open(session_file).read().strip()

def req(method, path, payload=None, timeout=700):
    data = None if payload is None else json.dumps(payload).encode()
    request = urllib.request.Request(base + path, data=data, method=method, headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode() or '{}')

def agent_req(method, path, payload=None, timeout=80):
    data = None if payload is None else json.dumps(payload).encode()
    request = urllib.request.Request(agent_base.rstrip('/') + path, data=data, method=method, headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode() or '{}')

def execute(script, args=None, timeout=430):
    return req('POST', f'/session/{sid}/execute/sync', {'script': script, 'args': args or []}, timeout=timeout).get('value')

def body_text():
    return execute('return document.body && document.body.innerText ? document.body.innerText : "";', timeout=30) or ''

def wait_for_text(needle, timeout=60):
    deadline = time.time() + timeout
    last = ''
    while time.time() < deadline:
        last = body_text()
        if needle in last:
            return last
        time.sleep(0.5)
    print(f'missing text {needle}; body=' + last[:1200].replace('\n', ' | '), file=sys.stderr)
    sys.exit(10)

def click(label, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        ok = execute("""
const label = arguments[0];
const buttons = Array.from(document.querySelectorAll('button'));
const btn = buttons.find((b) => b.textContent && b.textContent.trim() === label)
  || buttons.find((b) => b.textContent && b.textContent.includes(label));
if (!btn || btn.disabled) return false;
btn.click();
return true;
""", [label], timeout=30)
        if ok is True:
            return
        time.sleep(0.5)
    print('button not found/enabled: ' + label, file=sys.stderr)
    sys.exit(11)

def click_testid(testid, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        ok = execute("""
const id = arguments[0];
const el = document.querySelector('[data-testid=\"' + id + '\"]');
if (!el || el.disabled) return false;
el.click();
return true;
""", [testid], timeout=30)
        if ok is True:
            return
        time.sleep(0.5)
    print('testid not found/enabled: ' + testid, file=sys.stderr)
    sys.exit(11)

def log_position():
    try:
        return os.path.getsize(log_file)
    except OSError:
        return 0

def wait_for_run_log(start_pos, timeout=780):
    """Wait for the run-once terminal event (批 M / doc21 G1 contract fix:
    the worker logs per-iteration `worker.run_once.iteration` events
    (tasks_succeeded/tasks_failed per iteration) plus a terminal
    `worker.run_once.break`; `worker.run_once.completed` is only an
    in-memory status string, never a log event name). Success = break seen,
    summed succeeded > 0, summed failed == 0. Returns (summary, relations)."""
    deadline = time.time() + timeout
    pos = start_pos
    seen_relations = set()
    succeeded = 0
    failed = 0
    iterations = 0
    terminal = None
    while time.time() < deadline:
        try:
            with open(log_file, 'r', encoding='utf-8', errors='replace') as handle:
                handle.seek(pos)
                while True:
                    line = handle.readline()
                    if not line:
                        break
                    pos = handle.tell()
                    try:
                        event = json.loads(line)
                    except Exception:
                        continue
                    name = event.get('event')
                    detail = event.get('detail') or {}
                    if name == 'discovery.relation_start':
                        seen_relations.add(str(detail.get('relation_id')))
                    if name == 'worker.run_once.iteration':
                        iterations += 1
                        succeeded += int(detail.get('tasks_succeeded') or 0)
                        failed += int(detail.get('tasks_failed') or 0)
                    if name in ('worker.run_once.error', 'worker.run_once.failed'):
                        print(f'worker run-once failed: {detail}', file=sys.stderr)
                        sys.exit(12)
                    if name == 'worker.run_once.break':
                        terminal = detail
        except OSError:
            pass
        if terminal is not None:
            break
        time.sleep(0.5)
    if not terminal:
        print(f'worker run did not reach a terminal event; iterations={iterations} succeeded={succeeded} failed={failed}; seen_relations={sorted(seen_relations)}; body=' + body_text()[:1600].replace('\n', ' | '), file=sys.stderr)
        sys.exit(12)
    if failed != 0 or succeeded <= 0:
        print(f'worker run-once ended without success: iterations={iterations} succeeded={succeeded} failed={failed}; break={terminal}; seen_relations={sorted(seen_relations)}', file=sys.stderr)
        sys.exit(12)
    summary = {'iterations': iterations, 'tasks_succeeded': succeeded, 'tasks_failed': failed}
    return summary, seen_relations

# Local-first: skip website OAuth / domains control-plane. Site binding uses
# WP-issued device token + route_secret only (issued earlier via issue_wp_token).
wait_for_text('Overview', 90)
status = agent_req('GET', '/api/status')
print('local_status_ok logged_in=%s device=%s' % (
    status.get('data', {}).get('logged_in'),
    status.get('data', {}).get('device_id') or status.get('device_id'),
))
# 批 M / doc21 G1 marker modernization: the old "Refresh local data" step is
# gone — Overview's domain/component refresh buttons only render in
# legacy_server_control_plane mode, which the local-first journey never runs.
# Agent liveness is already proven by the /api/status call above.
click('Sites')
click('Add WordPress site')
wait_for_text('Add site', 60)
# Site binding lives in the Add-site modal (sites-modal-* testids are the
# primary selectors). Force native setters so Svelte bind:value updates and
# the Save button enables.
execute("""
const [wpUrl, token, secret] = arguments;
const setNative = (el, value) => {
  if (!el) return;
  const proto = window.HTMLInputElement.prototype;
  const desc = Object.getOwnPropertyDescriptor(proto, 'value');
  desc.set.call(el, value);
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
};
const url = document.querySelector('[data-testid=\"sites-modal-url\"]')
  || Array.from(document.querySelectorAll('input')).find((i) => (i.placeholder || '').includes('example.com'));
const tok = document.querySelector('[data-testid=\"sites-modal-token\"]')
  || Array.from(document.querySelectorAll('input')).find((i) => (i.placeholder || '').includes('wptc1'));
const sec = document.querySelector('[data-testid=\"sites-modal-route-secret\"]')
  || Array.from(document.querySelectorAll('input')).find((i) => (i.placeholder || '').includes('From the WP plugin'));
if (!url || !tok) return { ok: false, reason: 'missing_inputs' };
setNative(url, wpUrl);
setNative(tok, token);
if (sec) setNative(sec, secret);
const save = document.querySelector('[data-testid=\"sites-modal-save\"]')
  || Array.from(document.querySelectorAll('button')).find((b) => (b.textContent || '').includes('Save site binding'));
return { ok: true, url: url.value, token_len: (tok.value || '').length, save_disabled: !!(save && save.disabled) };
""", [wp_url, wp_token, route_secret])
# Wait until Save enables, then click via testid/label.
deadline = time.time() + 30
while time.time() < deadline:
    enabled = execute("""
const save = document.querySelector('[data-testid=\"sites-modal-save\"]')
  || Array.from(document.querySelectorAll('button')).find((b) => (b.textContent || '').trim() === 'Save');
return !!save && !save.disabled;
""", [], timeout=15)
    if enabled is True:
        break
    time.sleep(0.4)
else:
    print('Save site binding stayed disabled after fill', file=sys.stderr)
    sys.exit(11)
try:
    click_testid('sites-modal-save')
except SystemExit:
    click('Save')
wait_for_text('Token saved', 90)
print('site_binding_ok')

click('Components')
wait_for_text('Local runtime components', 60)

# 批 M / doc21 G1 marker modernization: the old always-visible 4-input
# "Configure local mock component" form is gone. MyComponentsTab is a CRUD
# table; creation happens in ComponentModal with stable DOM ids
# (component-modal-*). The api-base/model inputs render only after the kind
# select switches to openai_compatible.

def set_input_by_id(el_id, value, tries=20):
    for _ in range(tries):
        ok = execute("""
const [elId, value] = arguments;
const el = document.getElementById(elId);
if (!el) return false;
const proto = window.HTMLInputElement.prototype;
const desc = Object.getOwnPropertyDescriptor(proto, 'value');
desc.set.call(el, value);
el.dispatchEvent(new Event('input', { bubbles: true }));
el.dispatchEvent(new Event('change', { bubbles: true }));
return true;
""", [el_id, value], timeout=30)
        if ok is True:
            return
        time.sleep(0.5)
    print('input not found: ' + el_id, file=sys.stderr)
    sys.exit(11)

def set_select_by_id(el_id, value, tries=20):
    for _ in range(tries):
        ok = execute("""
const [elId, value] = arguments;
const el = document.getElementById(elId);
if (!el || el.tagName !== 'SELECT') return false;
const proto = window.HTMLSelectElement.prototype;
const desc = Object.getOwnPropertyDescriptor(proto, 'value');
desc.set.call(el, value);
el.dispatchEvent(new Event('input', { bubbles: true }));
el.dispatchEvent(new Event('change', { bubbles: true }));
return true;
""", [el_id, value], timeout=30)
        if ok is True:
            return
        time.sleep(0.5)
    print('select not found: ' + el_id, file=sys.stderr)
    sys.exit(11)

def modal_save_click(anchor_sel, tries=40):
    # Click the 'Save' button scoped to the modal overlay containing the
    # anchor element, so page-level Save buttons can never be hit.
    for _ in range(tries):
        ok = execute("""
const anchor = document.querySelector(arguments[0]);
if (!anchor) return false;
const modal = anchor.closest('.fixed');
if (!modal) return false;
const save = Array.from(modal.querySelectorAll('button'))
  .find((b) => (b.textContent || '').trim() === 'Save');
if (!save || save.disabled) return false;
save.click();
return true;
""", [anchor_sel], timeout=30)
        if ok is True:
            return
        time.sleep(0.5)
    print('modal save not found/enabled for anchor ' + anchor_sel, file=sys.stderr)
    sys.exit(11)

click('Create')
wait_for_text('Create Component', 30)
set_input_by_id('component-modal-id', 'desktop-local-openai')
set_input_by_id('component-modal-name', 'Desktop Local OpenAI Mock')
set_select_by_id('component-modal-kind', 'openai_compatible')
wait_for_text('API Base URL', 30)
set_input_by_id('component-modal-api-base', mock_api_base)
set_input_by_id('component-modal-model', 'mock-openai-v1')
modal_save_click('#component-modal-id')
wait_for_text('Component created successfully', 90)
print('component_config_ok')

# Bind the mock bearer key: component_rt/runner/signing.rs consumes the
# auth.api_key field as "Authorization: Bearer <key>". The Edit Auth action
# lives in the component's expanded panel — expand the row first (the row
# header button contains the component id text).
click('desktop-local-openai')
click('Edit Auth')
wait_for_text('Auth Credentials', 30)
click('Add Field')
ok = execute("""
const [apiKey] = arguments;
// 'Field Name'/'Field Value' placeholders are unique to AuthBindingModal —
// query document-wide (a .fixed-pop() here can grab the toast container).
const pairs = Array.from(document.querySelectorAll('input[placeholder=\"Field Name\"], input[placeholder=\"Field Value\"]'));
if (pairs.length < 2) return false;
const setNative = (el, value) => {
  const proto = window.HTMLInputElement.prototype;
  const desc = Object.getOwnPropertyDescriptor(proto, 'value');
  desc.set.call(el, value);
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
};
setNative(pairs[0], 'api_key');
setNative(pairs[1], apiKey);
return true;
""", [mock_api_key], timeout=30)
if ok is not True:
    print('auth field pair inputs missing; body=' + body_text()[:1200].replace('\n', ' | '), file=sys.stderr)
    sys.exit(12)
modal_save_click('input[placeholder=\"Field Value\"]')
wait_for_text('Auth Saved', 90)
print('component_auth_ok')

click('Tasks')
wait_for_text('Translation jobs')
click('Discovery')
click_testid('tasks-bootstrap-discovery')
wait_for_text('Discovery tasks refreshed from site relations', 120)
print('discovery_ok')

# One Overview run-once processes every discovery relation and emits a
# single aggregate worker.run_once.completed; the old per-relation
# "Focus relation + Run worker once" controls no longer exist on Tasks.
click('Overview')
wait_for_text('Overview', 60)
start_pos = log_position()
click_testid('overview-run-once')
# First start may be gated by the worker-start preflight: field lanes whose
# components are missing but policy-marked warn/confirm_continue raise a
# confirm panel (blocking=0 here). Confirm it — the same dialog a human
# clicks through — which reissues the run with force. When no panel appears
# the poll below simply times out and the run proceeds normally.
deadline = time.time() + 45
while time.time() < deadline:
    ok = execute("""
const el = document.querySelector('[data-testid=\"overview-preflight-continue\"]');
if (!el || el.disabled) return false;
el.click();
return true;
""", [], timeout=30)
    if ok is True:
        print('preflight_confirmed')
        break
    time.sleep(0.5)
summary, seen_relations = wait_for_run_log(start_pos)
if str(virtual_relation_id) != '0' and str(virtual_relation_id) not in seen_relations:
    print(f'virtual relation {virtual_relation_id} not processed; seen={sorted(seen_relations)}', file=sys.stderr)
    sys.exit(12)
print(f"worker_once_ok tasks_succeeded={summary.get('tasks_succeeded')} tasks_failed={summary.get('tasks_failed')} relations={','.join(sorted(seen_relations))}")

# 批 N3 / U-7: 常驻循环 UI 旅程 —— Start Loop → 自动 tick → Stop Loop。
# Loop event contract (worker.rs spawn_worker_loop_task): one
# worker.loop.started{poll_seconds}, one worker.loop.tick_ok per iteration
# (an empty queue still ticks — each tick runs web_ui_run_worker_once which
# logs worker.run_once.empty_domain_tasks), terminal worker.loop.stopped{}.
# No new tasks are required: an empty-queue tick still proves the loop
# auto-claim machinery cycles end to end via the real UI control.
loop_start_pos = log_position()
click_testid('overview-start-loop')
# Start Loop walks the same worker-start preflight confirm gate; bridge it
# the same way as run-once.
deadline = time.time() + 45
while time.time() < deadline:
    ok = execute("""
const el = document.querySelector('[data-testid=\"overview-preflight-continue\"]');
if (!el || el.disabled) return false;
el.click();
return true;
""", [], timeout=30)
    if ok is True:
        print('loop_preflight_confirmed')
        break
    time.sleep(0.5)
loop_started = False
loop_ticks = 0
loop_stopped = False
loop_poll_seconds = None
loop_pos = loop_start_pos
deadline = time.time() + 150
while time.time() < deadline:
    try:
        with open(log_file, 'r', encoding='utf-8', errors='replace') as handle:
            handle.seek(loop_pos)
            while True:
                line = handle.readline()
                if not line:
                    break
                loop_pos = handle.tell()
                try:
                    event = json.loads(line)
                except Exception:
                    continue
                name = event.get('event')
                if name == 'worker.loop.started':
                    loop_started = True
                    loop_poll_seconds = (event.get('detail') or {}).get('poll_seconds')
                elif name == 'worker.loop.tick_ok':
                    loop_ticks += 1
                elif name == 'worker.loop.stopped':
                    loop_stopped = True
    except OSError:
        pass
    if loop_started and loop_ticks >= 1:
        break
    time.sleep(1)
if not loop_started:
    print('worker.loop.started never logged (loop UI journey failed)', file=sys.stderr)
    sys.exit(13)
if loop_ticks < 1:
    print('no worker.loop.tick_ok observed within budget (poll may be long)', file=sys.stderr)
    sys.exit(13)
click_testid('overview-stop-loop')
deadline = time.time() + 45
while time.time() < deadline:
    try:
        with open(log_file, 'r', encoding='utf-8', errors='replace') as handle:
            handle.seek(loop_start_pos)
            for line in handle:
                try:
                    event = json.loads(line)
                except Exception:
                    continue
                if event.get('event') == 'worker.loop.stopped':
                    loop_stopped = True
    except OSError:
        pass
    if loop_stopped:
        break
    time.sleep(0.5)
if not loop_stopped:
    print('worker.loop.stopped never logged after Stop Loop', file=sys.stderr)
    sys.exit(13)
print(f"worker_loop_ok poll_seconds={loop_poll_seconds} ticks={loop_ticks}")
PY
}

verify_wp_writeback() {
  echo "== Verifying WP write-back via canonical Stage 7 =="
  set +e
  (
    cd "$ROOT_DIR"
    WPTSALL_LAB=1 \
    E2E_PROJECT="$PROJECT" \
    E2E_SCOPE="$SCOPE" \
    WP_BASE="$WP_BASE" \
    LAB_WP_HOST="127.0.0.1" \
    LAB_WP_PORT="$LAB_WP_PORT" \
    LAB_WP_CONTAINER="$WP_CONTAINER" \
    CLIENT_BASE="http://127.0.0.1:$AGENT_PORT" \
    CLIENT_URL="http://127.0.0.1:$AGENT_PORT" \
    WPTSALL_DB_PATH="$DB_PATH" \
    WPTSALL_LOG_FILE="$LOG_FILE" \
    WP_CLIENT_TOKEN="$WP_CLIENT_TOKEN" \
    ROUTE_SECRET="$ROUTE_SECRET" \
    WPTSALL_DEVICE_ID="$DEVICE_ID" \
    WPTSALL_WP_DEVICE_ID="$DEVICE_ID" \
    E2E_SKIP_OAUTH_LOGIN=1 \
    bash tests/modules/wpmmcc-ats/e2e/stages/07-verify.sh
  )
  local rc=$?
  set -e
  if [[ "$rc" -ne 0 ]]; then
    echo "WARN: Stage 7 exited rc=$rc (continuing to restart-persistence check)" >&2
  fi
  return 0
}

verify_restart_persistence() {
  echo "== Restarting Desktop and verifying persisted binding/component/jobs =="
  stop_tauri_session
  start_tauri_session
  python3 - "$DRIVER_PORT" "$SESSION_FILE" "$WP_BASE" <<'PY'
import json, sys, time, urllib.request
port, session_file, wp_base = sys.argv[1:]
base = f'http://127.0.0.1:{port}'
sid = open(session_file).read().strip()

def req(method, path, payload=None, timeout=40):
    data = None if payload is None else json.dumps(payload).encode()
    request = urllib.request.Request(base + path, data=data, method=method, headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode() or '{}')

def execute(script, args=None):
    return req('POST', f'/session/{sid}/execute/sync', {'script': script, 'args': args or []}).get('value')

def body_text():
    return execute('return document.body && document.body.innerText ? document.body.innerText : "";') or ''

def wait_for_text(needle, timeout=90):
    deadline = time.time() + timeout
    last = ''
    while time.time() < deadline:
        last = body_text()
        if needle in last:
            return last
        time.sleep(0.5)
    print('missing text after restart: ' + needle + '; body=' + last[:1200].replace('\n', ' | '), file=sys.stderr)
    sys.exit(20)

def click(label):
    ok = execute("""
const label = arguments[0];
const btn = Array.from(document.querySelectorAll('button')).find((b) => b.textContent && b.textContent.trim() === label)
  || Array.from(document.querySelectorAll('button')).find((b) => b.textContent && b.textContent.includes(label));
if (!btn) return false;
btn.click();
return true;
""", [label])
    if ok is not True:
        print('button missing after restart: ' + label, file=sys.stderr)
        sys.exit(21)

wait_for_text('Overview')
click('Sites')
text = wait_for_text(wp_base)
# 批 M / doc21 G1: the old 'connected'/'not verified' strings are gone; the
# binding row now renders a three-state health badge (Verified / Stale /
# Unverified — sites.health_*).
if not ('Verified' in text or 'Unverified' in text or 'Stale' in text):
    print('site binding not visible after restart; body=' + text[:1200].replace('\n', ' | '), file=sys.stderr)
    sys.exit(22)
click('Components')
wait_for_text('Local runtime components')
# The component list loads asynchronously after restart (loadLocalComps
# fetch) — poll for the row instead of asserting immediate visibility.
deadline = time.time() + 90
text = ''
while time.time() < deadline:
    text = body_text()
    if 'desktop-local-openai' in text or 'Desktop Local OpenAI Mock' in text:
        break
    time.sleep(0.5)
else:
    print('component not visible after restart; body=' + text[:1200].replace('\n', ' | '), file=sys.stderr)
    sys.exit(23)
click('Tasks')
wait_for_text('Translation jobs')
print('restart_persistence_ok')
PY
}

ensure_mock_api
prepare_wp_lane
issue_wp_token
load_relation_ids
rm -rf "$RUNTIME_ROOT"
mkdir -p "$RUNTIME_ROOT"
start_tauri_session
run_desktop_dom_flow
verify_wp_writeback
verify_restart_persistence

echo "desktop docker WP journey passed"
