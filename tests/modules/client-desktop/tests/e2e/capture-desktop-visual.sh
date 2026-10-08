#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/../../../../lib/repo-root.sh"
ROOT_DIR="$(wptsall_repo_root "${SCRIPT_DIR}")"
FRONTEND_DIR="$ROOT_DIR/client-desktop/frontend"
APP_BIN="${WPTSALL_DESKTOP_APP:-$ROOT_DIR/client-desktop/src-tauri/target/debug/wptsall-desktop}"
DRIVER_PORT="${WPTSALL_TAURI_DRIVER_PORT:-4448}"
NATIVE_PORT="${WPTSALL_TAURI_NATIVE_PORT:-4449}"
VITE_PORT="1430"
NATIVE_DRIVER="${WPTSALL_TAURI_NATIVE_DRIVER:-$(command -v WebKitWebDriver || true)}"
TMP_DIR="$(mktemp -d)"
OUT_DIR="$ROOT_DIR/tests/reports/e2e/client-desktop/visual-audit"
mkdir -p "$OUT_DIR"
VITE_LOG="$TMP_DIR/vite.log"
DRIVER_LOG="$TMP_DIR/tauri-driver.log"
SESSION_FILE="$TMP_DIR/session-id"
VITE_PID=""
DRIVER_PID=""

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
  wait "$DRIVER_PID" "$VITE_PID" >/dev/null 2>&1 || true
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

# Ensure port 1430 is free
fuser -k 1430/tcp >/dev/null 2>&1 || true
fuser -k 4448/tcp >/dev/null 2>&1 || true
fuser -k 4449/tcp >/dev/null 2>&1 || true
sleep 0.5

echo "Starting Vite on $VITE_PORT..."
setsid bash -lc "cd '$FRONTEND_DIR' && exec npm run dev -- --host 127.0.0.1 --port '$VITE_PORT'" >"$VITE_LOG" 2>&1 &
VITE_PID=$!

echo "Starting tauri-driver on $DRIVER_PORT..."
setsid xvfb-run -a tauri-driver \
  --port "$DRIVER_PORT" \
  --native-port "$NATIVE_PORT" \
  --native-driver "$NATIVE_DRIVER" \
  >"$DRIVER_LOG" 2>&1 &
DRIVER_PID=$!

python3 - "$APP_BIN" "$DRIVER_PORT" "$VITE_PORT" "$SESSION_FILE" "$OUT_DIR" <<'PY'
import json, os, sys, time, urllib.request, urllib.error, base64

app = os.path.abspath(sys.argv[1])
driver_port = sys.argv[2]
vite_port = sys.argv[3]
session_file = sys.argv[4]
out_dir = sys.argv[5]
base = f'http://127.0.0.1:{driver_port}'

def wait_url(url: str, label: str, attempts: int = 80) -> None:
    last = None
    for _ in range(attempts):
        try:
            urllib.request.urlopen(url, timeout=1).read(128)
            return
        except Exception as exc:
            last = exc
            time.sleep(0.25)
    print(f'{label} did not become ready: {last}', file=sys.stderr)
    sys.exit(3)

def request(method: str, path: str, payload=None, timeout: int = 25):
    data = None if payload is None else json.dumps(payload).encode()
    req = urllib.request.Request(
        base + path,
        data=data,
        method=method,
        headers={'Content-Type': 'application/json'},
    )
    with urllib.request.urlopen(req, timeout=timeout) as response:
        raw = response.read().decode() or '{}'
        return json.loads(raw)

wait_url(f'http://127.0.0.1:{vite_port}/', 'Vite dev server')
wait_url(base + '/status', 'tauri-driver')

payload = {
    'capabilities': {
        'alwaysMatch': {
            'browserName': 'wry',
            'tauri:options': {
                'application': app,
                'args': [],
            },
        },
    },
}

session = request('POST', '/session', payload, timeout=40)
sid = session.get('value', {}).get('sessionId') or session.get('sessionId')
if not sid:
    print(f'missing session id: {session}', file=sys.stderr)
    sys.exit(5)
open(session_file, 'w').write(sid)

def exec_js(script, args=[]):
    return request(
        'POST',
        f'/session/{sid}/execute/sync',
        {'script': script, 'args': args},
        timeout=15,
    ).get('value')

def body_text() -> str:
    return exec_js('return document.body && document.body.innerText ? document.body.innerText : "";') or ''

def shot(name: str):
    res = request('GET', f'/session/{sid}/screenshot')
    data = res.get('value', '')
    file_path = os.path.join(out_dir, f'{name}.png')
    with open(file_path, 'wb') as f:
        f.write(base64.b64decode(data))
    print(f'desktop screenshot saved: {file_path}')

# Wait for overview
for _ in range(80):
    text = body_text()
    if ('Overview' in text or '概览' in text or 'Worker' in text) and 'Could not connect' not in text:
        break
    time.sleep(0.5)

print('Desktop window ready with content!')
time.sleep(1)
shot('01-overview')

def nav_click(label_regex):
    script = """
    const re = new RegExp(arguments[0], 'i');
    const btns = Array.from(document.querySelectorAll('button, a'));
    const btn = btns.find(b => re.test((b.textContent || b.innerText || '').trim()));
    if (btn) { btn.click(); return true; }
    return false;
    """
    res = exec_js(script, [label_regex])
    time.sleep(0.8)
    return res

def close_modal():
    exec_js("""
    const cancel = Array.from(document.querySelectorAll('button')).find(b => /取消|Cancel|关闭|Close/i.test(b.innerText || ''));
    if (cancel) cancel.click();
    """)
    time.sleep(0.5)

# 2. Sites
if nav_click('Sites|站点'):
    shot('02-sites')
    exec_js("""
    const addBtn = document.querySelector('[data-testid="sites-add-site"]') ||
                   Array.from(document.querySelectorAll('button')).find(b => /添加|Add/i.test(b.innerText || ''));
    if (addBtn) addBtn.click();
    """)
    time.sleep(0.8)
    shot('02d-sites-add-modal')
    close_modal()

# 3. Components
if nav_click('Components|组件'):
    shot('03-components')
    # Click create component if exists
    exec_js("""
    const btn = Array.from(document.querySelectorAll('button')).find(b => /创建组件|添加组件|Add Component/i.test(b.innerText || ''));
    if (btn) btn.click();
    """)
    time.sleep(0.8)
    shot('03b-component-modal')
    close_modal()

# 4. API Keys
if nav_click('API Keys|密钥|服务商'):
    shot('04-apikeys')
    # Click add key
    exec_js("""
    const btn = Array.from(document.querySelectorAll('button')).find(b => /添加密钥|新建密钥|Add Key/i.test(b.innerText || ''));
    if (btn) btn.click();
    """)
    time.sleep(0.8)
    shot('04b-apikeys-key-modal')
    close_modal()

# 5. Tasks
if nav_click('Tasks|任务'):
    shot('05-tasks')
    for label, sname in [('Discovery|发现|任务配置', '05b-tasks-discovery'), ('Pending|待审', '05c-tasks-pending'), ('Sync|跨站同步', '05d-tasks-sync-pairs'), ('Jobs|翻译任务', '05e-tasks-jobs')]:
        if nav_click(label):
            shot(sname)
    # While on Sync Pairs, click "+ 新建同步对"
    nav_click('Sync|跨站同步')
    time.sleep(0.6)
    exec_js("""
    const btn = Array.from(document.querySelectorAll('button')).find(b => /新建同步对|添加同步对/i.test(b.innerText || ''));
    if (btn) btn.click();
    """)
    time.sleep(0.8)
    shot('05d-sync-pair-modal')
    close_modal()

    # Click 配对管理
    exec_js("""
    const btn = Array.from(document.querySelectorAll('button')).find(b => /配对管理|Pairing/i.test(b.innerText || ''));
    if (btn) btn.click();
    """)
    time.sleep(0.8)
    shot('05d-pairing-modal')
    close_modal()

# 6. Logs
if nav_click('Logs|日志'):
    shot('06-logs')

# 7. History
if nav_click('History|历史'):
    shot('07-history')

# 8. Settings
if nav_click('Settings|设置'):
    shot('08-settings')
    exec_js("""
    const workerTab = document.querySelector('[data-testid="settings-tab-worker"]') ||
                      Array.from(document.querySelectorAll('button')).find(b => /Worker/i.test(b.innerText || ''));
    if (workerTab) workerTab.click();
    """)
    time.sleep(0.8)
    shot('08b-settings-worker')

print('ALL DESKTOP SCREENSHOTS (INCLUDING MODALS) CAPTURED SUCCESSFULLY!')
PY
chmod +x tests/modules/client-desktop/tests/e2e/capture-desktop-visual.sh
