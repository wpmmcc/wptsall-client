#!/usr/bin/env bash
# Install, update-check, and uninstall one published native package on the
# runner that matches its platform. User data is a sentinel written after the
# process has stopped; uninstall must leave that file unchanged and remove
# the package files.
set -euo pipefail

VERSION="${VERSION:?}"
PRODUCT="${PRODUCT:?}"
PLATFORM="${PLATFORM:?}"
KIND="${KIND:?}"
PUB='RWRK0RTOL1wb3YAJ6sywCLsN0kvpOttJ1CYv3AHluZAfYUXzSEm/0cSw'
EVIDENCE="${GITHUB_WORKSPACE}/native-lifecycle.json"
export EVIDENCE VERSION PRODUCT PLATFORM KIND
PY="$(command -v python3 || command -v python)"
work="${RUNNER_TEMP}/native-lifecycle"
data="${work}/userdata"
log="${work}/client.log"
pidfile="${work}/client.pid"
mkdir -p "$work" "$data"
echo "Owned runner lifecycle sentinel. Not user content." > "$data/preservation-sentinel.txt"

stage="setup"
install_ok=false
update_ok=false
update_action=""
uninstall_ok=false
data_preserved=false

write_evidence() {
  STAGE="$stage" INSTALL_OK="$install_ok" UPDATE_OK="$update_ok" \
  UPDATE_ACTION="$update_action" UNINSTALL_OK="$uninstall_ok" \
  DATA_PRESERVED="$data_preserved" "$PY" - <<'PY'
import json, os
from pathlib import Path
def flag(name):
    return os.environ[name] == "true"
Path(os.environ["EVIDENCE"]).write_text(json.dumps({
    "format": "runner-native-lifecycle-v1",
    "version": os.environ["VERSION"],
    "product": os.environ["PRODUCT"],
    "platform": os.environ["PLATFORM"],
    "kind": os.environ["KIND"],
    "runner_os": os.environ.get("RUNNER_OS", ""),
    "runner_arch": os.environ.get("RUNNER_ARCH", ""),
    "stage": os.environ["STAGE"],
    "install": "passed" if flag("INSTALL_OK") else "not_passed",
    "update_check": "passed" if flag("UPDATE_OK") else "not_passed",
    "update_action": os.environ.get("UPDATE_ACTION") or "",
    "uninstall": "passed" if flag("UNINSTALL_OK") else "not_passed",
    "per_user_data_preserved": flag("DATA_PRESERVED"),
}, indent=2) + "\n")
PY
}
trap write_evidence EXIT

fail() {
  echo "FAIL: $*" >&2
  if [ -f "$log" ]; then
    echo "---- client log ----" >&2
    tail -n 80 "$log" >&2 || true
  fi
  exit 1
}

actual="$(echo "$RUNNER_OS-$RUNNER_ARCH" | tr '[:upper:]' '[:lower:]' \
  | sed 's/macos/darwin/;s/x64/x86_64/;s/arm64/aarch64/')"
[ "$actual" = "$PLATFORM" ] || fail "runner $actual does not match $PLATFORM"

case "$PRODUCT:$KIND" in
  webui:deb|webui:dmg|webui:nsis|desktop:deb|desktop:dmg|desktop:nsis) ;;
  *) fail "unsupported product/kind $PRODUCT/$KIND" ;;
esac

if [ "$PRODUCT" = webui ]; then
  stem="wptsall-client-webui-${VERSION}-${PLATFORM}"
  pkg=wptsall-client-webui
else
  stem="wptsall-client-${VERSION}-${PLATFORM}"
  pkg=wptsall-client
fi
case "$KIND" in
  deb) asset="${stem}.deb" ;;
  dmg) asset="${stem}.dmg" ;;
  nsis) asset="${stem}-setup.exe" ;;
esac

install_minisign() {
  case "$RUNNER_OS" in
    Linux)
      sudo rm -f /etc/apt/sources.list.d/google-chrome* || true
      sudo sed -i '/dl\.google\.com/d' /etc/apt/sources.list 2>/dev/null || true
      sudo apt-get update || true
      sudo apt-get install -y --no-install-recommends minisign
      ;;
    macOS)
      brew install minisign
      ;;
    Windows)
      case "$RUNNER_ARCH" in
        X64|AMD64) arch=x86_64 ;;
        ARM64) arch=aarch64 ;;
        *) fail "unsupported Windows arch $RUNNER_ARCH" ;;
      esac
      curl -fsSL "https://github.com/jedisct1/minisign/releases/download/0.12/minisign-0.12-win64.zip" \
        -o "$work/minisign.zip"
      unzip -o "$work/minisign.zip" -d "$work/minisign-bin"
      bin="$work/minisign-bin/minisign-win64/$arch/minisign.exe"
      [ -f "$bin" ] || fail "minisign.exe missing"
      mkdir -p /usr/local/bin
      cp "$bin" /usr/local/bin/minisign.exe
      export PATH="/usr/local/bin:$PATH"
      ;;
  esac
  minisign -v
}

install_minisign
mkdir -p "$work/assets"
gh release download "v${VERSION}" --repo "$GITHUB_REPOSITORY" \
  -p "$asset" -p "${asset}.minisig" -D "$work/assets"
minisign -Vm "$work/assets/$asset" -P "$PUB" -x "$work/assets/$asset.minisig"
echo "signature verified: $asset"

stop_client() {
  if [ -f "$pidfile" ]; then
    pid="$(tr -d '[:space:]' < "$pidfile")"
    if [ "$RUNNER_OS" = Windows ]; then
      powershell.exe -NoProfile -Command "Stop-Process -Id $pid -Force -ErrorAction SilentlyContinue" || true
    else
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
    rm -f "$pidfile"
  fi
  if [ "$RUNNER_OS" = Windows ]; then
    powershell.exe -NoProfile -Command "Get-Process wptsall-client,wptsall-client-webui -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue; exit 0" || true
  fi
}

wait_status() {
  ready=0
  for _ in $(seq 1 90); do
    if curl -fsS "http://127.0.0.1:8977/api/status" -o "$work/status.json" 2>/dev/null \
        && grep -q '"success":true' "$work/status.json"; then
      ready=1
      break
    fi
    sleep 1
  done
  [ "$ready" = 1 ] || fail "installed client did not answer /api/status"
}

start_client() {
  export WPTSALL_DATA_DIR="$data"
  export WPTSALL_DB_PATH="$data/client.db"
  export WPTSALL_LOG_FILE="$log"
  case "$RUNNER_OS" in
    Linux)
      if [ "$PRODUCT" = desktop ]; then
        sudo apt-get install -y --no-install-recommends xvfb >/dev/null
        xvfb-run -a /usr/bin/"$pkg" >"$log" 2>&1 &
      else
        /usr/bin/"$pkg" >"$log" 2>&1 &
      fi
      echo $! > "$pidfile"
      ;;
    macOS)
      xattr -dr com.apple.quarantine "$app" 2>/dev/null || true
      if [ "$PRODUCT" = webui ]; then
        "$app/Contents/MacOS/launch-webui" >"$log" 2>&1 &
      else
        "$app/Contents/MacOS/wptsall-client" >"$log" 2>&1 &
      fi
      echo $! > "$pidfile"
      ;;
    Windows)
      data_win="$(cygpath -w "$data")"
      log_win="$(cygpath -w "$log")"
      export WPTSALL_DATA_DIR="$data_win"
      export WPTSALL_DB_PATH="${data_win}\\client.db"
      export WPTSALL_LOG_FILE="$log_win"
      if [ "$PRODUCT" = webui ]; then
        exe="$(cygpath -w "$install_root/launch-webui.cmd")"
      else
        exe="$(cygpath -w "$install_root/bin/wptsall-client.exe")"
      fi
      pid_win="$(cygpath -w "$pidfile")"
      powershell.exe -NoProfile -Command "\$p = Start-Process -FilePath '${exe}' -PassThru; Set-Content -Path '${pid_win}' -Value \$p.Id -Encoding ascii"
      ;;
  esac
}

stage="install"
case "$KIND" in
  deb)
    sudo apt-get update || true
    sudo apt-get install -y "$work/assets/$asset"
    [ -x "/usr/bin/$pkg" ] || fail "launcher /usr/bin/$pkg missing after install"
    ;;
  dmg)
    mnt="$work/mnt"
    mkdir -p "$mnt"
    hdiutil attach -nobrowse -mountpoint "$mnt" "$work/assets/$asset"
    app_src="$(find "$mnt" -maxdepth 2 -name '*.app' -print -quit)"
    [ -n "$app_src" ] || fail "dmg has no .app"
    app="$work/$(basename "$app_src")"
    cp -R "$app_src" "$app"
    hdiutil detach "$mnt"
    ;;
  nsis)
    install_root="$(cygpath -u "$LOCALAPPDATA/Programs/$pkg")"
    MSYS_NO_PATHCONV=1 "$work/assets/$asset" /S
    for _ in $(seq 1 60); do
      [ -f "$install_root/uninstall.exe" ] && break
      sleep 1
    done
    [ -f "$install_root/uninstall.exe" ] || fail "NSIS install did not produce uninstall.exe"
    [ -f "$install_root/bin/wptsall-client.exe" ] || fail "installed binary missing"
    ;;
esac
install_ok=true
echo "install ok: $PRODUCT $PLATFORM"

stage="update"
start_client
wait_status
curl -fsS "http://127.0.0.1:8977/api/update-check" -o "$work/update-check.json" \
  || fail "update-check request failed"
"$PY" - "$work/update-check.json" "$VERSION" <<'PY'
import json, sys
body = json.load(open(sys.argv[1], encoding="utf-8"))
data = body.get("data") or {}
if body.get("success") is not True:
    raise SystemExit("update-check did not succeed: " + json.dumps(body)[:500])
if data.get("current_version") != sys.argv[2] or data.get("latest_version") != sys.argv[2]:
    raise SystemExit("update-check versions: " + json.dumps(data)[:500])
if data.get("update_available") is not False:
    raise SystemExit("installed release must already be current: " + json.dumps(data)[:500])
print("update-check ok", data.get("product_id"), data.get("current_version"))
PY
update_ok=true
curl -sS -o "$work/perform-update.json" -X POST "http://127.0.0.1:8977/api/perform-update" \
  || true
"$PY" - "$work/perform-update.json" <<'PY'
import json, sys
body = json.load(open(sys.argv[1], encoding="utf-8"))
code = (body.get("error") or {}).get("code")
if code != "ALREADY_UP_TO_DATE":
    raise SystemExit("perform-update: " + json.dumps(body)[:500])
print("perform-update ok", code)
PY
update_action="already_up_to_date"
echo "update ok: already current $VERSION"

stage="uninstall"
stop_client
sleep 1
before="$("$PY" - "$data/preservation-sentinel.txt" <<'PY'
import hashlib, sys
print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())
PY
)"
case "$KIND" in
  deb)
    sudo dpkg --remove "$pkg"
    [ ! -e "/opt/$pkg/bin/wptsall-client" ] || fail "package binary survived uninstall"
    [ ! -e "/usr/bin/$pkg" ] || fail "launcher survived uninstall"
    ;;
  dmg)
    rm -rf "$app"
    [ ! -e "$app" ] || fail "app survived uninstall"
    ;;
  nsis)
    MSYS_NO_PATHCONV=1 "$(cygpath -w "$install_root/uninstall.exe")" /S \
      || fail "Windows uninstaller exited $?"
    for _ in $(seq 1 60); do
      [ ! -e "$install_root/bin/wptsall-client.exe" ] && break
      sleep 1
    done
    [ ! -e "$install_root/bin/wptsall-client.exe" ] || fail "Windows binary survived uninstall"
    ;;
esac
after="$("$PY" - "$data/preservation-sentinel.txt" <<'PY'
import hashlib, sys
print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())
PY
)"
[ "$before" = "$after" ] || fail "uninstall changed per-user data"
[ -f "$data/preservation-sentinel.txt" ] || fail "per-user sentinel missing after uninstall"
data_preserved=true
uninstall_ok=true
stage="passed"
echo "PASS $PRODUCT $PLATFORM install update uninstall"
