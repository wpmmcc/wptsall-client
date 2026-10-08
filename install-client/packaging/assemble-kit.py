#!/usr/bin/env python3
"""Assemble production runner builds into the shared source-free kit format."""
from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

INSTALL = Path(__file__).resolve().parents[1]
PLATFORMS = {f"{os_}-{arch}" for os_ in ("linux", "windows", "darwin")
             for arch in ("x86_64", "aarch64")}


def bash_executable() -> str:
    """Windows PATH bash is the WSL launcher. Packaging scripts need Git Bash."""
    if os.name == "nt":
        candidate = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "Git" / "bin" / "bash.exe"
        if candidate.is_file():
            return str(candidate)
    return "bash"


def assemble(product: str, binary: Path, ui: Path, version: str, platform: str,
             triple: str, out: Path) -> Path:
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) or platform not in PLATFORMS:
        raise ValueError("exact version and supported platform are required")
    if not re.fullmatch(r"[a-zA-Z0-9_-]+", triple):
        raise ValueError("invalid Rust host triple")
    if product not in {"webui", "desktop"}:
        raise ValueError("invalid product")
    if binary.is_symlink() or not binary.is_file() or binary.stat().st_size == 0:
        raise ValueError("a regular nonempty built binary is required")
    if ui.is_symlink() or not ui.is_dir() or not (ui / "index.html").is_file():
        raise ValueError("a built production UI is required")
    for path in ui.rglob("*"):
        if path.is_symlink() or (not path.is_file() and not path.is_dir()):
            raise ValueError("UI aliases and special members are refused")
    out.mkdir(parents=True, exist_ok=True)
    asset = out / f"kit-{product}-{platform}.tar.gz"
    if asset.exists() or asset.is_symlink():
        raise FileExistsError(asset)
    root_name = "wptsall-client-webui" if product == "webui" else "wptsall-client"
    with tempfile.TemporaryDirectory(prefix="assemble-kit-", dir=out) as temporary:
        tree = Path(temporary) / root_name
        (tree / "bin").mkdir(parents=True)
        entry = tree / "bin" / ("wptsall-client.exe" if platform.startswith("windows-")
                                else "wptsall-client")
        shutil.copy2(binary, entry)
        entry.chmod(0o755)
        shutil.copytree(ui, tree / "ui" / product)
        for name, value in {"VERSION": version, "VERSION-BINARY": version,
                            "VERSION-WEBUI": version, "PLATFORM.txt": platform,
                            "TRIPLE.txt": triple}.items():
            (tree / name).write_text(value + "\n", encoding="utf-8")
        (tree / "HARDENING.txt").write_text(
            f"product_entry=wptsall-client\nversion={version}\nplatform={platform}\n"
            "strip=build-profile\nobfuscation=none\n", encoding="utf-8")
        shutil.copy2(INSTALL / "shared/PRODUCTION-FIRST-LAUNCH-AUTHORIZATION.md",
                     tree / "FIRST-LAUNCH-AUTHORIZATION.md")
        (tree / "FIRST-LAUNCH-AUTHORIZATION.md").chmod(0o644)
        bash = bash_executable()
        harden = INSTALL / "shared/secure-harden-release.sh"
        subprocess.run([bash, str(harden), str(tree)], check=True)
        obfuscate = INSTALL / "hooks/obfuscate-kit.sh"
        if obfuscate.is_file():
            subprocess.run([bash, str(obfuscate), str(tree)], check=True)
        else:
            (tree / "OBFUSCATION.txt").write_text("diversify=skipped\n", encoding="utf-8")
        (tree / "HARDENING.txt").write_text(
            "strip=build-profile\nobfuscation="
            + ("applied" if (tree / "OBFUSCATION.txt").is_file() else "none")
            + f"\nproduct_entry=wptsall-client\nversion={version}\nplatform={platform}\n",
            encoding="utf-8")
        if not platform.startswith("windows-"):
            launcher = tree / "bin" / ("wptsall-webui-start" if product == "webui"
                                       else "wptsall-start")
            launcher.write_text(
                '#!/bin/sh\nset -eu\numask 077\n'
                'DIR="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"\n'
                'export WPTSALL_INSTALL_ROOT="$DIR"\n'
                'export WPTSALL_DATA_DIR="${WPTSALL_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/'
                + root_name + '}"\n'
                + ('export WPTSALL_WEB_UI=1\n' if product == "webui" else '')
                + 'exec "$DIR/bin/wptsall-client" "$@"\n', encoding="utf-8")
            launcher.chmod(0o755)
        sums = "".join(
            f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.relative_to(tree).as_posix()}\n"
            for path in sorted(tree.rglob("*")) if path.is_file())
        (tree / "SHA256SUMS").write_text(sums, encoding="utf-8")
        temporary_asset = Path(temporary) / asset.name
        with tarfile.open(temporary_asset, "w:gz") as archive:
            archive.add(tree, arcname=root_name)
        subprocess.run([sys.executable, str(INSTALL / "packaging/validate-kit.py"),
                        product, temporary_asset.as_posix(), "--version", version], check=True)
        with asset.open("xb") as target, temporary_asset.open("rb") as source:
            shutil.copyfileobj(source, target)
    return asset


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("product", choices=["webui", "desktop"])
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--ui", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--triple", required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    print(assemble(args.product, args.binary, args.ui, args.version, args.platform,
                   args.triple, args.out))


if __name__ == "__main__":
    main()
