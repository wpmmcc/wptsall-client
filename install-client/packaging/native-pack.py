#!/usr/bin/env python3
"""Package validated, source-free kits without rebuilding or touching user data."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile

INSTALL = Path(__file__).resolve().parents[1]


def run(*args: str, **kwargs) -> None:
    subprocess.run(args, check=True, **kwargs)


def write(path: Path, text: str, mode: int = 0o644) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8") as stream:
        stream.write(text)
    path.chmod(mode)


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def deb(tree: Path, stage: Path, asset: Path, product: str, version: str, arch: str) -> None:
    name = "wptsall-client-webui" if product == "webui" else "wptsall-client"
    root = stage / "deb"
    shutil.copytree(tree, root / "opt" / name)
    deps = "" if product == "webui" else "libwebkit2gtk-4.1-0, libgtk-3-0"
    write(root / "DEBIAN/control", f"""Package: {name}
Version: {version}
Section: utils
Priority: optional
Architecture: {arch}
Maintainer: WPMM <info@wpmm.cc>
Depends: {deps}
Description: WPTSALL {product} client
 Local-first WordPress client. Uninstall preserves per-user configuration and data.
""".replace("Depends: \n", ""))
    launcher = f"""#!/bin/sh
set -eu
umask 077
export WPTSALL_DATA_DIR="${{WPTSALL_DATA_DIR:-${{XDG_DATA_HOME:-$HOME/.local/share}}/{name}}}"
"""
    if product == "webui":
        launcher += 'export WPTSALL_WEB_UI=1\nexport WPTSALL_WEB_UI_BIND=127.0.0.1:8977\n'
    launcher += f'exec /opt/{name}/bin/wptsall-client "$@"\n'
    write(root / "usr/bin" / name, launcher, 0o755)
    write(root / "usr/share/applications" / f"{name}.desktop", f"""[Desktop Entry]
Name=WPTSALL {product.capitalize()}
Exec={name}
Terminal={'true' if product == 'webui' else 'false'}
Type=Application
Categories=Utility;
""")
    # No maintainer scripts, automatic service enable, or user-data removal.
    run("dpkg-deb", "--root-owner-group", "--build", str(root), str(asset))
    run("dpkg-deb", "--info", str(asset))
    run("dpkg-deb", "--contents", str(asset))


def create_dmg(volume: Path, stage: Path, asset: Path, title: str) -> None:
    if asset.exists():
        raise FileExistsError("native package output exists")
    for attempt in range(3):
        # Failed images remain in the owned temporary stage, never at the
        # publication path. Only macOS's transient busy error is retried.
        candidate = stage / f"image-{attempt}.dmg"
        result = subprocess.run(
            ["hdiutil", "create", "-volname", title, "-srcfolder", str(volume),
             "-format", "UDZO", str(candidate)], capture_output=True, text=True)
        print(result.stdout, end="")
        print(result.stderr, end="", file=sys.stderr)
        if result.returncode == 0:
            run("hdiutil", "verify", str(candidate))
            with asset.open("xb") as destination, candidate.open("rb") as source:
                shutil.copyfileobj(source, destination)
            return
        if "Resource busy" not in result.stderr or attempt == 2:
            result.check_returncode()
        time.sleep(2 * (attempt + 1))


def dmg(tree: Path, stage: Path, asset: Path, product: str, version: str, auth: Path) -> None:
    title = f"WPTSALL {product.capitalize()}"
    volume = stage / "volume"
    app = volume / f"{title}.app" / "Contents"
    (app / "MacOS").mkdir(parents=True)
    shutil.copytree(tree, app / "Resources" / "kit")
    if product == "desktop":
        shutil.copy2(tree / "bin/wptsall-client", app / "MacOS/wptsall-client")
        executable = "wptsall-client"
    else:
        executable = "launch-webui"
        write(app / "MacOS" / executable, """#!/bin/sh
set -eu
umask 077
HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
export WPTSALL_DATA_DIR="${WPTSALL_DATA_DIR:-$HOME/Library/Application Support/cc.wpmm.wptsall-webui}"
export WPTSALL_WEB_UI=1
export WPTSALL_WEB_UI_BIND=127.0.0.1:8977
exec "$HERE/../Resources/kit/bin/wptsall-client" "$@"
""", 0o755)
    with (app / "Info.plist").open("xb") as stream:
        plistlib.dump({
            "CFBundleName": title,
            "CFBundleDisplayName": title,
            "CFBundleIdentifier": f"cc.wpmm.wptsall-{product}",
            "CFBundleVersion": version,
            "CFBundleShortVersionString": version,
            "CFBundleExecutable": executable,
            "CFBundlePackageType": "APPL",
            "LSMinimumSystemVersion": "11.0",
        }, stream)
    shutil.copy2(auth, volume / auth.name)
    (volume / "Applications").symlink_to("/Applications", target_is_directory=True)
    create_dmg(volume, stage, asset, title)


def nsis_path(path: Path | str) -> str:
    # Native Windows NSIS uses backslash paths for File/OutFile. Converting
    # drive paths to forward slashes breaks its file enumeration.
    value = str(path)
    if '"' in value or "$" in value or "\n" in value or "\r" in value:
        raise ValueError("unsupported NSIS package path")
    return value


def nsis(tree: Path, stage: Path, asset: Path, product: str, version: str) -> None:
    name = f"WPTSALL {product.capitalize()}"
    ident = "wptsall-client-webui" if product == "webui" else "wptsall-client"
    # Per-user, non-elevated installation. A manifest enumerates only package
    # members for uninstall; config/data created by the app are never removed.
    files = sorted(path for path in tree.rglob("*") if path.is_file())
    install = ['SetOutPath "$INSTDIR"']
    uninstall = []
    for path in files:
        relative = path.relative_to(tree)
        parent = str(relative.parent).replace("/", "\\")
        install += [f'SetOutPath "$INSTDIR\\{parent}"', f'File "{nsis_path(path)}"']
        uninstall.append(f'Delete "$INSTDIR\\{str(relative).replace("/", chr(92))}"')
    folders = sorted({parent for path in files for parent in path.relative_to(tree).parents},
                     key=lambda p: (len(p.parts), str(p)), reverse=True)
    uninstall += [f'RMDir "$INSTDIR\\{str(path).replace("/", chr(92))}"' for path in folders]
    args = ''
    if product == "webui":
        write(stage / "launch-webui.cmd", f"""@echo off
setlocal
if not defined WPTSALL_DATA_DIR set "WPTSALL_DATA_DIR=%LOCALAPPDATA%\\{ident}\\data"
set "WPTSALL_WEB_UI=1"
set "WPTSALL_WEB_UI_BIND=127.0.0.1:8977"
"%~dp0bin\\wptsall-client.exe" %*
""")
        install += ['SetOutPath "$INSTDIR"', f'File "{nsis_path(stage / "launch-webui.cmd")}"']
        uninstall += ['Delete "$INSTDIR\\launch-webui.cmd"']
        command = r"$INSTDIR\launch-webui.cmd"
    else:
        command = r"$INSTDIR\bin\wptsall-client.exe"
    script = f"""Unicode true
RequestExecutionLevel user
Name "{name}"
OutFile "{nsis_path(asset)}"
InstallDir "$LOCALAPPDATA\\Programs\\{ident}"
!include "MUI2.nsh"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
Section
SetShellVarContext current
{chr(10).join(install)}
WriteUninstaller "$INSTDIR\\uninstall.exe"
CreateShortcut "$SMPROGRAMS\\{name}.lnk" "{command}" "{args}"
WriteRegStr HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{ident}" "DisplayName" "{name}"
WriteRegStr HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{ident}" "DisplayVersion" "{version}"
WriteRegStr HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{ident}" "UninstallString" '$\\"$INSTDIR\\uninstall.exe$\\"'
SectionEnd
Section "Uninstall"
SetShellVarContext current
{chr(10).join(uninstall)}
Delete "$SMPROGRAMS\\{name}.lnk"
Delete "$INSTDIR\\uninstall.exe"
RMDir "$INSTDIR"
DeleteRegKey HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{ident}"
SectionEnd
"""
    path = stage / "installer.nsi"
    write(path, script)
    run("makensis", str(path))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("product", choices=["webui", "desktop"])
    parser.add_argument("--kit", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--kinds", default="tree")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", args.version):
        parser.error("version must be an exact three-part version")
    kit, out = args.kit.resolve(strict=True), args.out.resolve()
    run(sys.executable, str(INSTALL / "packaging/validate-kit.py"), args.product,
        kit.as_posix(), "--version", args.version)
    kinds = args.kinds.split(",")
    if len(set(kinds)) != len(kinds) or any(k not in {"tree", "portable", "deb", "dmg", "nsis"} for k in kinds):
        parser.error("supported kinds: tree,portable,deb,dmg,nsis")
    for kind, tool in [("deb", "dpkg-deb"), ("dmg", "hdiutil"), ("nsis", "makensis")]:
        if kind in kinds and not shutil.which(tool):
            parser.error(f"{kind} requires {tool}; no package created")
    out.mkdir(parents=True, exist_ok=True)
    auth = INSTALL / "shared/PRODUCTION-FIRST-LAUNCH-AUTHORIZATION.md"
    if not auth.is_file():
        parser.error("first-launch authorization document is missing")
    with tempfile.TemporaryDirectory(prefix=f"native-{args.product}-", dir=out.parent) as temporary:
        stage = Path(temporary)
        with tarfile.open(kit) as archive:
            archive.extractall(stage, filter="data")
        root_name = "wptsall-client-webui" if args.product == "webui" else "wptsall-client"
        tree = stage / root_name
        platform = (tree / "PLATFORM.txt").read_text().strip()
        if platform not in {f"{os_}-{arch}" for os_ in ["linux", "darwin", "windows"] for arch in ["x86_64", "aarch64"]}:
            parser.error("kit platform is invalid")
        for kind, os_ in [("deb", "linux"), ("dmg", "darwin"), ("nsis", "windows")]:
            if kind in kinds and not platform.startswith(os_ + "-"):
                parser.error(f"{kind} does not match kit platform")
        arch = "arm64" if platform.endswith("aarch64") else "amd64"
        prefix = f"{root_name}-{args.version}-{platform}"
        extensions = {"tree": ".tar.gz", "portable": "-portable.zip", "deb": ".deb", "dmg": ".dmg", "nsis": "-setup.exe"}
        assets = [out / (prefix + extensions[k]) for k in kinds]
        checksum = out / f"SHA256SUMS-{args.product}-{platform}.txt"
        record = out / f"PACKAGING-{args.product}-{platform}.json"
        if any(path.exists() for path in [*assets, checksum, record]):
            parser.error("current output already exists; use a fresh output directory")
        for kind, asset in zip(kinds, assets):
            if kind == "tree":
                with tarfile.open(asset, "w:gz") as archive:
                    archive.add(tree, arcname=root_name)
            elif kind == "portable":
                with zipfile.ZipFile(asset, "x", compression=zipfile.ZIP_DEFLATED) as archive:
                    for path in sorted(tree.rglob("*")):
                        if path.is_file():
                            archive.write(path, str(path.relative_to(stage)))
            elif kind == "deb":
                deb(tree, stage, asset, args.product, args.version, arch)
            elif kind == "dmg":
                dmg(tree, stage, asset, args.product, args.version, auth)
            elif kind == "nsis":
                nsis(tree, stage, asset, args.product, args.version)
            if not asset.is_file() or asset.stat().st_size == 0:
                raise RuntimeError(f"{kind} produced no package")
        auth_out = out / "README-AUTHORIZATION.md"
        if auth_out.exists() and auth_out.read_bytes() != auth.read_bytes():
            raise RuntimeError("existing authorization document differs")
        if not auth_out.exists():
            shutil.copy2(auth, auth_out)
            auth_out.chmod(0o644)
        write(checksum, "".join(f"{digest(path)}  {path.name}\n" for path in [*assets, auth_out]))
        write(record, json.dumps({
            "format": "source-free-native-pack-v1",
            "product": args.product, "version": args.version, "platform": platform,
            "kit_sha256": digest(kit), "assets": {p.name: digest(p) for p in assets},
            "native_install_validated": False, "user_data_removal": False,
        }, indent=2) + "\n")
        print(f"Packaged {len(assets)} assets. Native installation still requires platform acceptance.")


if __name__ == "__main__":
    main()
