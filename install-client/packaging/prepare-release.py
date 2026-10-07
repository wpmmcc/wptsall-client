#!/usr/bin/env python3
"""Verify twelve runner packages and prepare the signed four-axis update channel."""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile

PLATFORMS = tuple(f"{os_}-{arch}" for os_ in ("linux", "windows", "darwin")
                  for arch in ("x86_64", "aarch64"))
INSTALL = Path(__file__).resolve().parents[1]


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def prepare(assets: Path, version: str, repository: str, tag: str,
            key: Path, public_key: str) -> None:
    if (not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version)
            or not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository)
            or not re.fullmatch(r"[A-Za-z0-9_.-]+", tag)):
        raise ValueError("invalid release coordinates")
    if (assets / "releases.json").exists():
        raise FileExistsError("release output exists")
    original = {p.name for p in assets.iterdir()}
    expected = {"README-AUTHORIZATION.md"}
    for product in ("webui", "desktop"):
        for platform in PLATFORMS:
            kit = assets / f"kit-{product}-{platform}.tar.gz"
            record_path = assets / f"PACKAGING-{product}-{platform}.json"
            record = json.loads(record_path.read_bytes())
            if (record.get("format") != "source-free-native-pack-v1"
                    or record.get("product") != product or record.get("platform") != platform
                    or record.get("version") != version or record.get("kit_sha256") != sha(kit)
                    or record.get("user_data_removal") is not False):
                raise ValueError("runner package does not bind its exact source-free kit")
            root = "wptsall-client-webui" if product == "webui" else "wptsall-client"
            kind = ".deb" if platform.startswith("linux-") else (
                ".dmg" if platform.startswith("darwin-") else "-setup.exe")
            prefix = f"{root}-{version}-{platform}"
            names = {prefix + kind, prefix + "-portable.zip"}
            if set(record.get("assets", {})) != names:
                raise ValueError("both portable and matching native packages are required")
            for name in names:
                path = assets / name
                if path.is_symlink() or not path.is_file() or sha(path) != record["assets"][name]:
                    raise ValueError("runner package hash mismatch")
            subprocess.run(["bash", (INSTALL / "packaging/validate-kit.sh").as_posix(),
                            product, kit.as_posix(), "--version", version], check=True)
            with tarfile.open(kit) as archive:
                if archive.extractfile(root + "/PLATFORM.txt").read().decode().strip() != platform:
                    raise ValueError("kit platform mismatch")
            expected |= names | {kit.name, record_path.name,
                                  f"SHA256SUMS-{product}-{platform}.txt"}
    if original != expected or any(p.is_symlink() or not p.is_file() for p in assets.iterdir()):
        raise ValueError("unexpected, missing or aliased candidate assets")
    base = f"https://github.com/{repository}/releases/download/{tag}"
    products = {}
    for product, binary_axis, ui_axis in (
        ("webui", "client-wpplugin", "client-wpplugin-webui"),
        ("desktop", "client-desktop", "client-desktop-webui"),
    ):
        kit_names = [f"kit-{product}-{platform}.tar.gz" for platform in PLATFORMS]
        sums = assets / f"RELEASE-SHA256SUMS-{product}.txt"
        sums.write_text("".join(f"{sha(assets / name)}  {name}\n" for name in kit_names))
        bundle = assets / f"{product}-ui-{version}.tar.gz"
        root = "wptsall-client-webui" if product == "webui" else "wptsall-client"
        with tempfile.TemporaryDirectory() as temporary:
            stage = Path(temporary)
            with tarfile.open(assets / kit_names[0]) as archive:
                archive.extractall(stage, filter="data")
            tree = stage / root
            with (stage / "PRODUCT.txt").open("w") as stream:
                stream.write(product + "\n")
            with tarfile.open(bundle, "w:gz") as archive:
                archive.add(tree / "ui", arcname="ui")
                archive.add(tree / "VERSION-WEBUI", arcname="VERSION-WEBUI")
                archive.add(stage / "PRODUCT.txt", arcname="PRODUCT.txt")
        ui_sums = assets / f"SHA256SUMS-{product}-ui.txt"
        ui_sums.write_text(f"{sha(bundle)}  {bundle.name}\n")
        for axis, download, signature in (
            (binary_axis, f"kit-{product}-{{platform}}.tar.gz", sums.name),
            (ui_axis, f"{product}-ui-{{version}}.tar.gz", ui_sums.name),
        ):
            products[axis] = {
                "latest_version": version, "min_supported_version": "2.0.0",
                "release_notes_url": f"https://github.com/{repository}/releases/tag/{tag}",
                "download_url_template": f"{base}/{download}",
                "signature_url_template": f"{base}/{signature}.minisig", "mandatory": False,
            }
    data = {"schema_version": 1, "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "products": products, "signing": {
                "algorithm": "minisign", "public_key_b64": public_key}}
    (assets / "releases.json").write_text(json.dumps({"success": True, "data": data}, indent=2))
    # Sign every published asset, including native packages and package records.
    for path in sorted(assets.iterdir()):
        if path.suffix == ".minisig":
            raise ValueError("preexisting signatures are not accepted")
        subprocess.run(["minisign", "-S", "-s", str(key), "-m", str(path)], check=True)
        subprocess.run(["minisign", "-V", "-P", public_key, "-m", str(path)], check=True)
    # The client verifies canonical inner data, not the outer wire wrapper.
    with tempfile.TemporaryDirectory() as temporary:
        canonical = Path(temporary) / "canonical.json"
        canonical.write_bytes(json.dumps(data, sort_keys=True, ensure_ascii=False,
                                        separators=(",", ":"), allow_nan=False).encode())
        signature = assets / "releases.json.minisig"
        signature.unlink()
        subprocess.run(["minisign", "-S", "-s", str(key), "-m", str(canonical),
                        "-x", str(signature)], check=True)
        subprocess.run(["minisign", "-V", "-P", public_key, "-m", str(canonical),
                        "-x", str(signature)], check=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--assets", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--key", type=Path, required=True)
    parser.add_argument("--public-key", required=True)
    args = parser.parse_args()
    prepare(args.assets, args.version, args.repository, args.tag, args.key, args.public_key)


if __name__ == "__main__":
    main()
