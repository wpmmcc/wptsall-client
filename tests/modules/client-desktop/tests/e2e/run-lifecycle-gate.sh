#!/usr/bin/env bash
set -euo pipefail

# Process-level lifecycle gate for the Desktop client (G-16).
#
# Asserts, against the real binary, two behaviors implemented in
# client-desktop/src-tauri/src/lib.rs:
# 1. Single-instance (tauri_plugin_single_instance): a second launch while
#    the first runs must forward to the first instance and exit promptly,
#    leaving instance A alive.
# 2. Close-to-tray (CloseRequested -> prevent_close + hide): a window close
#    request must hide the window, NOT exit the process.
#
# Tooling notes:
# - runs the app under a private Xvfb display (no window manager);
# - xdotool/wmctrl are NOT available on this host, so the close request is
#   delivered as a raw WM_DELETE_WINDOW ClientMessage via python3 + ctypes
#   libX11 — the same X message a window manager sends. GDK translates it to
#   a delete-event -> Tauri CloseRequested regardless of WM presence;
# - the single-instance plugin needs a D-Bus session bus, so
#   DBUS_SESSION_BUS_ADDRESS must be set (a normal login session has it);
# - the debug binary is the default target because it tracks the current
#   (possibly uncommitted) lib.rs; the bundled release binary may be older.
#   The window is created before page load, so the Vite dev server is not
#   required for these process-level assertions.
#
# Non-goals: WebView DOM interaction (tauri-smoke.sh covers that) and
# tray-icon menu interaction.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../../../../lib/repo-root.sh
source "${SCRIPT_DIR}/../../../../lib/repo-root.sh"
ROOT_DIR="$(wptsall_repo_root "${SCRIPT_DIR}")"
APP_BIN="${WPTSALL_DESKTOP_APP:-$ROOT_DIR/client-desktop/src-tauri/target/debug/wptsall-desktop}"
REPORTS_DIR="${SCRIPT_DIR}/reports"
TMP_DIR="$(mktemp -d)"
XVFB_LOG="$TMP_DIR/xvfb.log"
APP_A_LOG="$TMP_DIR/instance-a.log"
APP_B_LOG="$TMP_DIR/instance-b.log"
XVFB_PID=""
APP_A_PID=""
DISPLAY_NUM=""

# kill -0 is zombie-blind (an unreaped exited child still answers), so
# liveness must exclude zombies: a crashed/finished app must be detectable.
proc_alive() {
  local state
  state="$(ps -o stat= -p "$1" 2>/dev/null | tr -d ' ')" || return 1
  [[ -n "$state" && "$state" != Z* ]]
}

cleanup() {
  if [[ -n "$APP_A_PID" ]]; then
    kill -TERM "$APP_A_PID" >/dev/null 2>&1 || true
  fi
  if [[ -n "$XVFB_PID" ]]; then
    kill -TERM -- "-$XVFB_PID" >/dev/null 2>&1 || kill "$XVFB_PID" >/dev/null 2>&1 || true
  fi
  wait "$APP_A_PID" "$XVFB_PID" >/dev/null 2>&1 || true

  if [[ "${WPTSALL_LIFECYCLE_GATE_KEEP_LOGS:-0}" == "1" ]]; then
    echo "logs kept in $TMP_DIR"
  else
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

fail() {
  local code="$1"
  shift
  echo "lifecycle gate FAILED: $*" >&2
  if [[ "${WPTSALL_LIFECYCLE_GATE_SHOW_LOGS:-0}" == "1" ]]; then
    echo '--- instance A log ---'
    tail -40 "$APP_A_LOG" 2>/dev/null || true
    echo '--- instance B log ---'
    tail -40 "$APP_B_LOG" 2>/dev/null || true
    echo '--- xvfb log ---'
    tail -20 "$XVFB_LOG" 2>/dev/null || true
  fi
  exit "$code"
}

need_tool() {
  local tool="$1"
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "missing required tool: $tool" >&2
    exit 2
  fi
}

need_tool Xvfb
need_tool xwininfo
need_tool python3
if [[ ! -x "$APP_BIN" ]]; then
  cat >&2 <<EOF
missing Desktop binary: $APP_BIN
Build it first (this gate defaults to the debug binary, which tracks the
current lib.rs); the router reuses the stable target and the shared cargo cache policy:
  bash scripts/wptsall.sh build desktop
Or point WPTSALL_DESKTOP_APP at an existing binary.
EOF
  exit 2
fi
if [[ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]]; then
  cat >&2 <<'EOF'
DBUS_SESSION_BUS_ADDRESS is not set: tauri_plugin_single_instance registers
via the D-Bus session bus on Linux, so the single-instance assertion cannot
run outside a D-Bus session. Start the gate from a normal login session or
export DBUS_SESSION_BUS_ADDRESS.
EOF
  exit 2
fi

# Pick a free private display number.
if [[ -n "${WPTSALL_LIFECYCLE_DISPLAY:-}" ]]; then
  DISPLAY_NUM="$WPTSALL_LIFECYCLE_DISPLAY"
else
  for n in 96 97 98 99 95 94 93; do
    if [[ ! -e "/tmp/.X11-unix/X$n" ]]; then
      DISPLAY_NUM="$n"
      break
    fi
  done
fi
if [[ -z "$DISPLAY_NUM" ]]; then
  echo "no free Xvfb display number (96-93 all in use); set WPTSALL_LIFECYCLE_DISPLAY" >&2
  exit 2
fi
export DISPLAY=":$DISPLAY_NUM"
# GTK attaches to the live Wayland compositor when WAYLAND_DISPLAY exists
# (the window then never lands on this gate's Xvfb). Force the X11 backend
# so both instances target the private display.
export GDK_BACKEND=x11
unset WAYLAND_DISPLAY

setsid Xvfb ":$DISPLAY_NUM" -screen 0 1280x900x24 -nolisten tcp \
  >"$XVFB_LOG" 2>&1 &
XVFB_PID=$!
for _ in $(seq 1 60); do
  if [[ -S "/tmp/.X11-unix/X$DISPLAY_NUM" ]]; then
    break
  fi
  if ! proc_alive "$XVFB_PID"; then
    fail 3 "Xvfb :$DISPLAY_NUM died on startup"
  fi
  sleep 0.25
done
[[ -S "/tmp/.X11-unix/X$DISPLAY_NUM" ]] || fail 3 "Xvfb :$DISPLAY_NUM did not create its socket"

find_window() {
  # First top-level window id whose title matches the app window title.
  xwininfo -root -tree 2>/dev/null | awk '/WPTSALL Translation Client/ {print $1; exit}'
}

# ---------------------------------------------------------------- instance A
"$APP_BIN" >"$APP_A_LOG" 2>&1 &
APP_A_PID=$!

WIN_ID=""
for _ in $(seq 1 240); do
  if ! proc_alive "$APP_A_PID"; then
    A_RC=0
    wait "$APP_A_PID" 2>/dev/null || A_RC=$?
    fail 3 "instance A exited during startup (rc=$A_RC); see $APP_A_LOG"
  fi
  WIN_ID="$(find_window)"
  if [[ -n "$WIN_ID" ]]; then
    break
  fi
  sleep 0.5
done
[[ -n "$WIN_ID" ]] || fail 3 "instance A window never appeared on $DISPLAY"
proc_alive "$APP_A_PID" || fail 3 "instance A is not alive"
echo "instance_a_ready pid=$APP_A_PID window=$WIN_ID"

# --------------------------------------------------- single-instance (G-16-1)
# Run the second launch in the foreground under timeout: the plugin's second
# instance calls ExecuteCallback on the first over D-Bus and exits with code
# 0 (tauri-plugin-single-instance-2.4.3/src/platform_impl/linux.rs:86).
# rc 124 from timeout means it did NOT exit on its own -> failure.
B_RC=0
timeout --signal=TERM 30 "$APP_BIN" >"$APP_B_LOG" 2>&1 || B_RC=$?
if [[ "$B_RC" == "124" ]]; then
  fail 4 "second instance did not exit within 30s (single-instance plugin inactive?)"
fi
proc_alive "$APP_A_PID" || fail 4 "first instance died after second launch"
if [[ -z "$(find_window)" ]]; then
  fail 4 "first instance window disappeared after second launch"
fi
echo "single_instance_ok second_instance_rc=$B_RC first_alive=yes"

# ---------------------------------------------------- close-to-tray (G-16-2)
if ! xwininfo -id "$WIN_ID" 2>/dev/null | grep -q 'Map State: IsViewable'; then
  fail 5 "instance A window $WIN_ID is not viewable before close"
fi

python3 - "$WIN_ID" <<'PY'
import ctypes
import sys

win_id = int(sys.argv[1], 0)

xlib = ctypes.CDLL("libX11.so.6")
xlib.XOpenDisplay.restype = ctypes.c_void_p
xlib.XOpenDisplay.argtypes = [ctypes.c_char_p]
xlib.XInternAtom.restype = ctypes.c_ulong
xlib.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
xlib.XSendEvent.argtypes = [
    ctypes.c_void_p,  # display
    ctypes.c_ulong,   # window
    ctypes.c_int,     # propagate
    ctypes.c_long,    # event mask
    ctypes.c_void_p,  # event
]
xlib.XFlush.argtypes = [ctypes.c_void_p]
xlib.XCloseDisplay.argtypes = [ctypes.c_void_p]


class XClientMessageEvent(ctypes.Structure):
    _fields_ = [
        ("type", ctypes.c_int),          # ClientMessage = 33
        ("serial", ctypes.c_ulong),
        ("send_event", ctypes.c_int),
        ("display", ctypes.c_void_p),
        ("window", ctypes.c_ulong),
        ("message_type", ctypes.c_ulong),  # WM_PROTOCOLS
        ("format", ctypes.c_int),          # 32
        ("data", ctypes.c_long * 5),       # l[0] = WM_DELETE_WINDOW
    ]


class XEvent(ctypes.Union):
    _fields_ = [("xclient", XClientMessageEvent), ("pad", ctypes.c_long * 24)]


display = xlib.XOpenDisplay(None)
if not display:
    print("cannot open display", file=sys.stderr)
    sys.exit(1)

protocols = xlib.XInternAtom(display, b"WM_PROTOCOLS", False)
delete_window = xlib.XInternAtom(display, b"WM_DELETE_WINDOW", False)
if not protocols or not delete_window:
    print("missing WM protocol atoms", file=sys.stderr)
    xlib.XCloseDisplay(display)
    sys.exit(1)

event = XEvent()
event.xclient.type = 33
event.xclient.display = display
event.xclient.window = win_id
event.xclient.message_type = protocols
event.xclient.format = 32
event.xclient.data[0] = delete_window

if not xlib.XSendEvent(display, win_id, False, 0, ctypes.byref(event)):
    print("XSendEvent failed", file=sys.stderr)
    xlib.XCloseDisplay(display)
    sys.exit(1)
xlib.XFlush(display)
xlib.XCloseDisplay(display)
PY
echo "close_requested_sent window=$WIN_ID"

# The close must hide the window and keep the process alive.
HIDDEN=0
for _ in $(seq 1 20); do
  # xwininfo prints the state as "IsUnMapped" (capital M).
  if xwininfo -id "$WIN_ID" 2>/dev/null | grep -qi 'Map State: IsUnmapped'; then
    HIDDEN=1
    break
  fi
  sleep 0.25
done
proc_alive "$APP_A_PID" || fail 5 "process exited after window close (close-to-tray not effective)"
if ! xwininfo -id "$WIN_ID" >/dev/null 2>&1; then
  fail 5 "window $WIN_ID was destroyed instead of hidden"
fi
if [[ "$HIDDEN" != "1" ]]; then
  fail 5 "window $WIN_ID stayed mapped after close (CloseRequested -> hide not effective)"
fi
echo "close_to_tray_ok process_alive=yes window_hidden=yes"

# ---------------------------------------------------------------------- report
TS="$(date +%Y%m%d-%H%M%S)"
mkdir -p "$REPORTS_DIR"
REPORT_FILE="$REPORTS_DIR/lifecycle-gate-$TS.json"
python3 - "$REPORT_FILE" "$APP_BIN" "$B_RC" <<'PY'
import json
import sys

report = {
    "gate": "client-desktop lifecycle (G-16)",
    "command": "bash tests/modules/client-desktop/tests/e2e/run-lifecycle-gate.sh",
    "binary": sys.argv[2],
    "result": "passed",
    "checks": {
        "single_instance_second_exits": True,
        "single_instance_first_alive": True,
        "single_instance_first_window_present": True,
        "second_instance_exit_code": json.loads(sys.argv[3]),
        "close_to_tray_process_alive": True,
        "close_to_tray_window_hidden": True,
        "close_to_tray_window_not_destroyed": True,
    },
}
with open(sys.argv[1], "w") as f:
    json.dump(report, f, indent=1)
PY
echo "report=$REPORT_FILE"
echo "lifecycle gate passed"
