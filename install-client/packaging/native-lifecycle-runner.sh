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

if [ "$PRODUCT" = desktop ]; then
  PORT=8978
else
  PORT=8977
fi

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
    "display_name": os.environ.get("BRAND_NAME", ""),
    "icon_sha256": os.environ.get("BRAND_ICON_SHA", ""),
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

brand_expected_name() {
  if [ "$PRODUCT" = webui ]; then
    printf '%s' "WPTSALL WebUI"
  else
    printf '%s' "WPTSALL Desktop"
  fi
}

render_brand_png() {
  icon_path="$1"
  title="$2"
  shot="${GITHUB_WORKSPACE}/native-lifecycle-brand.png"
  case "$RUNNER_OS" in
    Linux)
      sudo apt-get install -y --no-install-recommends python3-pil fonts-dejavu-core >/dev/null
      "$PY" - "$icon_path" "$title" "$shot" <<'PY'
import sys
from PIL import Image, ImageDraw, ImageFont
icon = Image.open(sys.argv[1]).convert("RGBA").resize((128, 128))
title, out = sys.argv[2], sys.argv[3]
canvas = Image.new("RGB", (360, 220), (36, 36, 36))
canvas.paste(icon, (116, 20), icon)
draw = ImageDraw.Draw(canvas)
font = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 18)
box = draw.textbbox((0, 0), title, font=font)
draw.text(((360 - (box[2] - box[0])) / 2, 164), title, fill=(255, 255, 255), font=font)
canvas.save(out)
PY
      ;;
    macOS)
      sips -s format png "$icon_path" --out "$work/brand-icon.png" >/dev/null
      cat > "$work/brand.swift" <<'SWIFT'
import AppKit
import Foundation
let icon = NSImage(contentsOfFile: CommandLine.arguments[1])!
let title = CommandLine.arguments[2]
let out = CommandLine.arguments[3]
let size = NSSize(width: 360, height: 220)
let image = NSImage(size: size)
image.lockFocus()
NSColor(calibratedWhite: 0.16, alpha: 1).setFill()
NSBezierPath(rect: NSRect(origin: .zero, size: size)).fill()
icon.draw(in: NSRect(x: 116, y: 64, width: 128, height: 128))
let attrs: [NSAttributedString.Key: Any] = [
    .font: NSFont.systemFont(ofSize: 18),
    .foregroundColor: NSColor.white
]
let text = NSString(string: title)
let textSize = text.size(withAttributes: attrs)
text.draw(at: NSPoint(x: (size.width - textSize.width) / 2, y: 28), withAttributes: attrs)
image.unlockFocus()
let rep = NSBitmapImageRep(data: image.tiffRepresentation!)!
try rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
SWIFT
      swift "$work/brand.swift" "$work/brand-icon.png" "$title" "$shot"
      ;;
    Windows)
      icon_win="$(cygpath -w "$icon_path")"
      shot_win="$(cygpath -w "$shot")"
      powershell.exe -NoProfile -Command "
        Add-Type -AssemblyName System.Drawing
        \$src = New-Object System.Drawing.Icon '${icon_win}'
        \$mark = \$src.ToBitmap()
        \$canvas = New-Object System.Drawing.Bitmap 360, 220
        \$g = [System.Drawing.Graphics]::FromImage(\$canvas)
        \$g.Clear([System.Drawing.Color]::FromArgb(36, 36, 36))
        \$g.DrawImage(\$mark, 116, 20, 128, 128)
        \$font = New-Object System.Drawing.Font 'Segoe UI', 14
        \$brush = [System.Drawing.Brushes]::White
        \$size = \$g.MeasureString('${title}', \$font)
        \$g.DrawString('${title}', \$font, \$brush, (360 - \$size.Width) / 2, 160)
        \$canvas.Save('${shot_win}', [System.Drawing.Imaging.ImageFormat]::Png)
      "
      ;;
  esac
  [ -s "$shot" ] || fail "brand screenshot was not written"
}

record_brand() {
  expected="$(brand_expected_name)"
  case "$KIND" in
    deb)
      entry="/usr/share/applications/${pkg}.desktop"
      BRAND_NAME="$(grep '^Name=' "$entry" | head -1 | cut -d= -f2-)"
      icon_key="$(grep '^Icon=' "$entry" | head -1 | cut -d= -f2-)"
      terminal="$(grep '^Terminal=' "$entry" | head -1 | cut -d= -f2-)"
      [ "$BRAND_NAME" = "$expected" ] || fail "menu name is '$BRAND_NAME', expected '$expected'"
      [ "$icon_key" = "$pkg" ] || fail "menu icon is '$icon_key'"
      [ "$terminal" = "false" ] || fail "menu entry opens in a terminal"
      icon="/usr/share/icons/hicolor/128x128/apps/${pkg}.png"
      [ -f "$icon" ] || fail "installed icon missing: $icon"
      if [ "$PRODUCT" = desktop ]; then
        [ ! -e "$HOME/.local/share/applications/wptsall-client.desktop" ] \
          || fail "old WPTSALL Client menu entry still hides the package"
        [ ! -e "$HOME/.local/share/applications/wptsall-desktop.desktop" ] \
          || fail "duplicate desktop menu entry remains"
      fi
      ;;
    dmg)
      BRAND_NAME="$("$PY" - "$app/Contents/Info.plist" <<'PY'
import plistlib, sys
info = plistlib.load(open(sys.argv[1], "rb"))
print(info.get("CFBundleDisplayName") or "")
PY
)"
      [ "$BRAND_NAME" = "$expected" ] || fail "bundle name is '$BRAND_NAME', expected '$expected'"
      icon="$app/Contents/Resources/AppIcon.icns"
      [ -f "$icon" ] || fail "AppIcon.icns missing"
      ;;
    nsis)
      programs="$APPDATA/Microsoft/Windows/Start Menu/Programs"
      # Start Menu paths are case-insensitive, so "WPTSALL WebUI.lnk" also
      # satisfies a test for the old "WPTSALL Webui.lnk". Compare the real name.
      shortcut_names="$(powershell.exe -NoProfile -Command "Get-ChildItem -LiteralPath '$(cygpath -w "$programs")' -Filter '*.lnk' | ForEach-Object { \$_.Name }")"
      shortcut_names="$(printf '%s\n' "$shortcut_names" | tr -d '\r')"
      printf '%s\n' "$shortcut_names" | grep -qx "${expected}.lnk" \
        || fail "Start Menu shortcut $expected missing"
      printf '%s\n' "$shortcut_names" | grep -qx "WPTSALL Client.lnk" \
        && fail "old WPTSALL Client shortcut remains"
      if [ "$expected" != "WPTSALL Webui" ]; then
        printf '%s\n' "$shortcut_names" | grep -qx "WPTSALL Webui.lnk" \
          && fail "old WPTSALL Webui shortcut remains"
      fi
      display="$(powershell.exe -NoProfile -Command "(Get-ItemProperty 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${pkg}').DisplayName")"
      display="$(printf '%s' "$display" | tr -d '\r')"
      [ "$display" = "$expected" ] || fail "uninstall display name is '$display'"
      BRAND_NAME="$expected"
      icon="$install_root/brand.ico"
      [ -f "$icon" ] || fail "brand.ico missing"
      lnk_icon="$(powershell.exe -NoProfile -Command "\$s=(New-Object -ComObject WScript.Shell).CreateShortcut('$(cygpath -w "$programs/$expected.lnk")'); \$s.IconLocation")"
      printf '%s' "$lnk_icon" | grep -q 'brand.ico' || fail "shortcut icon is '$lnk_icon'"
      ;;
  esac
  BRAND_ICON_SHA="$(sha256sum "$icon" 2>/dev/null | awk '{print $1}' || shasum -a 256 "$icon" | awk '{print $1}')"
  export BRAND_NAME BRAND_ICON_SHA
  render_brand_png "$icon" "$BRAND_NAME"
  echo "brand ok: $BRAND_NAME $BRAND_ICON_SHA"
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
    if curl -fsS "http://127.0.0.1:$PORT/api/status" -o "$work/status.json" 2>/dev/null \
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
    if [ "$PRODUCT" = desktop ]; then
      mkdir -p "$HOME/.local/share/applications"
      cat > "$HOME/.local/share/applications/wptsall-client.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=WPTSALL Client
Exec=/tmp/old-wptsall
Icon=wptsall-client
Terminal=false
EOF
      ln -sfn "$HOME/.local/share/applications/wptsall-client.desktop" \
        "$HOME/.local/share/applications/wptsall-desktop.desktop"
    fi
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
    programs="$APPDATA/Microsoft/Windows/Start Menu/Programs"
    mkdir -p "$programs"
    printf 'legacy shortcut\n' > "$programs/WPTSALL Client.lnk"
    printf 'legacy shortcut\n' > "$programs/WPTSALL Webui.lnk"
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
record_brand

stage="update"
start_client
wait_status
case "$RUNNER_OS" in
  Linux) ss -ltn | awk -v p=":$PORT\$" '$4 ~ p {found=1} END{exit found?0:1}' ;;
  macOS) lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null ;;
  Windows) powershell.exe -NoProfile -Command "if (-not (Get-NetTCPConnection -LocalPort $PORT -State Listen -ErrorAction SilentlyContinue)) { exit 1 }" ;;
esac || fail "installed client is not listening on $PORT"
if [ "$PRODUCT" = desktop ] && [ "$RUNNER_OS" = Linux ]; then
  if ss -ltn | awk '$4 ~ /:8977$/ {found=1} END{exit found?0:1}'; then
    fail "desktop took the WebUI port 8977"
  fi
fi
curl -fsS "http://127.0.0.1:$PORT/api/update-check" -o "$work/update-check.json" \
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
curl -sS -o "$work/perform-update.json" -X POST "http://127.0.0.1:$PORT/api/perform-update" \
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

if [ "$PRODUCT" = desktop ] && [ "$KIND" = deb ] && [ "$PLATFORM" = linux-x86_64 ]; then
  stage="coexist"
  web_asset="wptsall-client-webui-${VERSION}-${PLATFORM}.deb"
  gh release download "v${VERSION}" --repo "$GITHUB_REPOSITORY" \
    -p "$web_asset" -p "${web_asset}.minisig" -D "$work/assets"
  minisign -Vm "$work/assets/$web_asset" -P "$PUB" -x "$work/assets/$web_asset.minisig"
  sudo apt-get install -y "$work/assets/$web_asset"
  web_name="$(grep '^Name=' /usr/share/applications/wptsall-client-webui.desktop | head -1 | cut -d= -f2-)"
  web_icon="/usr/share/icons/hicolor/128x128/apps/wptsall-client-webui.png"
  [ "$web_name" = "WPTSALL WebUI" ] || fail "WebUI menu name is '$web_name'"
  [ -f "$web_icon" ] || fail "WebUI icon missing"
  web_sha="$(sha256sum "$web_icon" | awk '{print $1}')"
  [ "$web_sha" != "$BRAND_ICON_SHA" ] || fail "Desktop and WebUI icons are the same file"
  "$PY" - "/usr/share/icons/hicolor/128x128/apps/wptsall-client.png" "$web_icon" \
    "${GITHUB_WORKSPACE}/native-lifecycle-brand.png" <<'PY'
import sys
from PIL import Image, ImageDraw, ImageFont
font = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 18)
canvas = Image.new("RGB", (560, 220), (36, 36, 36))
draw = ImageDraw.Draw(canvas)
for index, (path, title) in enumerate((
    (sys.argv[1], "WPTSALL Desktop"),
    (sys.argv[2], "WPTSALL WebUI"),
)):
    icon = Image.open(path).convert("RGBA").resize((128, 128))
    x = 70 + index * 280
    canvas.paste(icon, (x, 20), icon)
    box = draw.textbbox((0, 0), title, font=font)
    draw.text((x + (128 - (box[2] - box[0])) / 2, 164), title, fill=(255, 255, 255), font=font)
canvas.save(sys.argv[3])
PY
  webdata="$work/webui-data"
  mkdir -p "$webdata"
  echo "WebUI coexistence sentinel." > "$webdata/preservation-sentinel.txt"
  WPTSALL_DATA_DIR="$webdata" \
  WPTSALL_DB_PATH="$webdata/client.db" \
  WPTSALL_LOG_FILE="$work/webui.log" \
    /usr/bin/wptsall-client-webui >"$work/webui-stdout.log" 2>&1 &
  echo $! > "$work/webui.pid"
  web_ready=0
  for _ in $(seq 1 90); do
    if curl -fsS "http://127.0.0.1:8977/api/status" >/dev/null 2>&1; then
      web_ready=1
      break
    fi
    sleep 1
  done
  [ "$web_ready" = 1 ] || fail "WebUI did not listen on 8977 while Desktop held $PORT"
  curl -fsS "http://127.0.0.1:$PORT/api/status" >/dev/null \
    || fail "Desktop stopped answering on $PORT after WebUI started"
  web_pid="$(cat "$work/webui.pid")"
  kill "$web_pid" 2>/dev/null || true
  wait "$web_pid" 2>/dev/null || true
  web_before="$("$PY" - "$webdata/preservation-sentinel.txt" <<'PY'
import hashlib, sys
print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())
PY
)"
  sudo dpkg --remove wptsall-client-webui
  [ ! -e /opt/wptsall-client-webui/bin/wptsall-client ] || fail "WebUI binary survived uninstall"
  web_after="$("$PY" - "$webdata/preservation-sentinel.txt" <<'PY'
import hashlib, sys
print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())
PY
)"
  [ "$web_before" = "$web_after" ] || fail "removing WebUI changed its per-user data"
  echo "coexist ok: Desktop :$PORT and WebUI :8977"
fi

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
