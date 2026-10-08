#!/usr/bin/env bash
set -euo pipefail

# Real Tauri window smoke for the Desktop UI.
#
# Scope:
# - launches the debug Desktop binary through tauri-driver in Xvfb;
# - serves the Vite dev URL required by debug Tauri builds;
# - verifies the real WebView DOM and top-level navigation.
#
# Non-goals:
# - this does not claim OAuth, WordPress write-back, or restart-persistence E2E;
# - those remain OPEN-05 until a full Lab-backed Desktop journey is green.

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
TMP_DIR="$(mktemp -d)"
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

  if [[ "${WPTSALL_DESKTOP_TAURI_SMOKE_KEEP_LOGS:-0}" == "1" ]]; then
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

setsid bash -lc "cd '$FRONTEND_DIR' && exec npm run dev -- --host 127.0.0.1 --port '$VITE_PORT'" >"$VITE_LOG" 2>&1 &
VITE_PID=$!
setsid xvfb-run -a tauri-driver \
  --port "$DRIVER_PORT" \
  --native-port "$NATIVE_PORT" \
  --native-driver "$NATIVE_DRIVER" \
  >"$DRIVER_LOG" 2>&1 &
DRIVER_PID=$!

python3 - "$APP_BIN" "$DRIVER_PORT" "$VITE_PORT" "$SESSION_FILE" <<'PY'
import json
import os
import sys
import time
import urllib.error
import urllib.request

app = os.path.abspath(sys.argv[1])
driver_port = sys.argv[2]
vite_port = sys.argv[3]
session_file = sys.argv[4]
base = f'http://127.0.0.1:{driver_port}'

def wait_url(url: str, label: str, attempts: int = 80) -> None:
    last = None
    for _ in range(attempts):
        try:
            urllib.request.urlopen(url, timeout=1).read(128)
            return
        except Exception as exc:  # noqa: BLE001 - report last connection error.
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
try:
    session = request('POST', '/session', payload, timeout=40)
except urllib.error.HTTPError as exc:
    print('session create failed:', exc.code, exc.read().decode(errors='replace'), file=sys.stderr)
    sys.exit(4)

sid = session.get('value', {}).get('sessionId') or session.get('sessionId')
if not sid:
    print(f'missing session id: {session}', file=sys.stderr)
    sys.exit(5)
open(session_file, 'w').write(sid)

def body_text() -> str:
    result = request(
        'POST',
        f'/session/{sid}/execute/sync',
        {
            'script': 'return document.body && document.body.innerText ? document.body.innerText : "";',
            'args': [],
        },
        timeout=20,
    )
    return result.get('value', '')

text = ''
for _ in range(80):
    text = body_text()
    # Accept EN or ZH chrome (Desktop may follow host locale).
    has_brand = 'WPTSALL' in text
    has_overview = 'Overview' in text or '概览' in text
    has_subtitle = (
        'Translation Client' in text
        or 'Translate & Sync' in text
        or '翻译' in text
    )
    if has_brand and has_overview and has_subtitle:
        break
    time.sleep(0.5)
else:
    print('desktop UI did not appear; body=' + text[:1000].replace('\n', ' | '), file=sys.stderr)
    sys.exit(6)

print('loaded=' + text[:250].replace('\n', ' | '))

nav_checks = [
    # (button labels EN/ZH, expected panel fragments EN/ZH)
    (('Sites', '站点'), ('Add WordPress site', '添加 WordPress 站点', '添加站点', 'WordPress')),
    (('Tasks', '任务'), ('Translation jobs', '翻译任务', 'Discovery', '任务配置')),
    (('Components', '翻译组件', '组件'), ('Local runtime components', '本地运行时组件', '组件')),
    (('Settings', '设置'), ('Version update', '版本更新', 'Worker', '审核')),
    # G-12a: secondary pages shared from WebUI via the @webui alias.
    # TranslationReview is intentionally excluded: it has no sidebar entry
    # (App.svelte only renders it with a live job itemId from Tasks), so it
    # is not reachable in a clean-data navigation sweep.
    (('API Keys', 'API 密钥'), ('API Keys', 'API 密钥', 'OAuth')),
    (('Logs', '日志'), ('JSONL', 'Load Logs', '加载日志')),
    # 窗口完整性批复核注记：侧栏按钮文案 en="History" / zh="翻译历史"
    # （nav.history 两语不对译直译）；旧断言只写 zh 文案，en 环境必败。
    # 页面判据用 subtitle（en "Browse and manage translation records" /
    # zh "浏览与管理翻译记录"）——页标题 en="History" 与侧栏按钮同名，
    # 单独引用无法证明页面真的切换。
    (('History', 'Translation History', '翻译历史'), ('Browse and manage translation records', '浏览与管理翻译记录', 'Clear Filters', 'No Records')),
]

for labels, expected_any in nav_checks:
    clicked = request(
        'POST',
        f'/session/{sid}/execute/sync',
        {
            'script': (
                "const labels = arguments[0];"
                "const btn = Array.from(document.querySelectorAll('button')).find((b) => {"
                "  const t = (b.textContent || '').trim();"
                "  return labels.some((l) => t === l || t.includes(l));"
                "});"
                "if (!btn) return false; btn.click(); return true;"
            ),
            'args': [list(labels)],
        },
        timeout=10,
    ).get('value')
    if clicked is not True:
        print(f'navigation button missing: {labels}', file=sys.stderr)
        sys.exit(7)

    label = labels[0]
    for _ in range(20):
        text = body_text()
        if any(frag in text for frag in expected_any):
            print(f'nav_ok={label}')
            break
        time.sleep(0.2)
    else:
        print(
            f'expected panel text missing after nav: {labels} / {expected_any}; body='
            + text[:1000].replace('\n', ' | '),
            file=sys.stderr,
        )
        sys.exit(8)

def js(script: str, args):
    return request(
        'POST',
        f'/session/{sid}/execute/sync',
        {'script': script, 'args': args},
        timeout=20,
    ).get('value')

# ── Window-integrity phase (窗口完整性批, 2026-09-25) ─────────────────────
# Tauri minWidth=800 sits below the shared Sidebar's lg(1024px) static
# breakpoint: resizing into 800–1023px pushes the sidebar off-canvas. The
# desktop shell must then expose the hamburger toggle (WebUI-shell parity),
# slide the sidebar back in, and auto-close it on navigate. Regression
# guard for the "window too narrow → nav unreachable" defect.
request('POST', f'/session/{sid}/window/rect', {'width': 800, 'height': 560}, timeout=20)
time.sleep(1.0)

narrow = js(
    "const nav = document.querySelector('nav[aria-label]');"
    "const navR = nav ? nav.getBoundingClientRect() : null;"
    "const burger = document.querySelector('header button[aria-expanded]');"
    "return {"
    "  navOffCanvas: !navR || navR.x <= -navR.width + 5,"
    "  burger: !!burger,"
    "  burgerVisible: !!burger && burger.getBoundingClientRect().width > 0"
    "};",
    [],
)
if not (narrow and narrow.get('navOffCanvas') and narrow.get('burgerVisible')):
    print(f'narrow-window shell broken: {narrow}', file=sys.stderr)
    sys.exit(9)
print('narrow_window_shell_ok=sidebar-off-canvas+hamburger-visible')

js("document.querySelector('header button[aria-expanded]').click(); return true;", [])
time.sleep(0.5)
opened = js(
    "const nav = document.querySelector('nav[aria-label]');"
    "const navR = nav.getBoundingClientRect();"
    "return { open: navR.x > -navR.width + 5,"
    "  backdrop: !!document.querySelector('div[role=\"presentation\"]') };",
    [],
)
if not (opened and opened.get('open') and opened.get('backdrop')):
    print(f'hamburger did not open sidebar: {opened}', file=sys.stderr)
    sys.exit(10)
print('narrow_window_hamburger_ok=sidebar-open+backdrop')

clicked_narrow = js(
    "const btn = Array.from(document.querySelectorAll('nav[aria-label] .flex-1 button'))"
    ".find((b) => (b.textContent || '').trim() === 'Sites' || (b.textContent || '').includes('站点'));"
    "if (!btn) return false; btn.click(); return true;",
    [],
)
if clicked_narrow is not True:
    print('narrow-window nav button missing after hamburger open', file=sys.stderr)
    sys.exit(11)
time.sleep(0.5)
closed = js(
    "const nav = document.querySelector('nav[aria-label]');"
    "const navR = nav.getBoundingClientRect();"
    "return { closed: navR.x <= -navR.width + 5,"
    "  backdropGone: !document.querySelector('div[role=\"presentation\"]') };",
    [],
)
if not (closed and closed.get('closed') and closed.get('backdropGone')):
    print(f'sidebar did not auto-close on navigate: {closed}', file=sys.stderr)
    sys.exit(12)
text = body_text()
if not any(frag in text for frag in ('Add WordPress site', '添加 WordPress 站点', '添加站点', 'WordPress')):
    print(f'narrow-window Sites panel missing after nav; body={text[:400]}', file=sys.stderr)
    sys.exit(13)
print('narrow_window_nav_ok=sites-panel+sidebar-auto-closed')

# Restore the default window size so the log tail stays representative.
request('POST', f'/session/{sid}/window/rect', {'width': 1024, 'height': 680}, timeout=20)

print('tauri real-window smoke passed')
PY
rc=$?

if [[ $rc -ne 0 || "${WPTSALL_DESKTOP_TAURI_SMOKE_SHOW_LOGS:-0}" == "1" ]]; then
  echo '--- vite log ---'
  tail -80 "$VITE_LOG" || true
  echo '--- tauri-driver log ---'
  tail -120 "$DRIVER_LOG" || true
fi

exit "$rc"
