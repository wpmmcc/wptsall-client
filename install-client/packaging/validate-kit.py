#!/usr/bin/env python3
"""Platform-independent source-free kit validation, shared by all packaging entrypoints."""
import argparse
import json
from pathlib import Path, PurePosixPath
import tarfile


def validate(product: str, kit: Path, expected_version: str = "") -> dict:
    if product not in {"webui", "desktop"}:
        raise ValueError("product must be webui or desktop")
    if kit.is_symlink() or not kit.is_file():
        raise ValueError("regular kit file required")
    root = "wptsall-client-webui" if product == "webui" else "wptsall-client"
    ui = f"{root}/ui/{product}/"
    metadata = {
        "VERSION", "VERSION-BINARY", "VERSION-WEBUI", "PLATFORM.txt", "TRIPLE.txt",
        "HARDENING.txt", "OBFUSCATION.txt", "SECURE-HARDEN-REPORT.txt", "SHA256SUMS",
        "FIRST-LAUNCH-AUTHORIZATION.md",
    }
    forbidden_parts = {
        ".git", "node_modules", "target", "src", "src-tauri", "Cargo.toml",
        "Cargo.lock", "data", "runtime", "logs", "tests", "test-results",
    }
    forbidden_names = {".env", ".env.local", ".env.production", "id_rsa", "credentials.json"}
    suffixes = (".rs", ".ts", ".svelte", ".map", ".pdb", ".dSYM", ".o", ".a")
    with tarfile.open(kit, "r:gz") as archive:
        members = archive.getmembers()
        if not members:
            raise ValueError("archive is empty")
        names = set()
        bad = []
        for member in members:
            name = member.name
            path = PurePosixPath(name)
            if (path.is_absolute() or ".." in path.parts or "\\" in name
                    or name.rstrip("/") != path.as_posix() or name.rstrip("/") in names):
                bad.append(f"unsafe, noncanonical or duplicate member: {name}")
            names.add(name.rstrip("/"))
            if not (member.isfile() or member.isdir()):
                bad.append(f"unsupported tar member type: {name}")
            if set(path.parts) & forbidden_parts:
                bad.append(f"forbidden member: {name}")
            basename = path.name.lower()
            if basename in forbidden_names or basename.endswith((".pem", ".key", ".p12", ".pfx")):
                bad.append(f"credential member: {name}")
            if name.endswith(suffixes):
                bad.append(f"source/build suffix: {name}")
            if name.rstrip("/") in {root, root + "/bin", root + "/ui", root + "/icons",
                                    ui.rstrip("/")}:
                continue
            if name.startswith((ui, root + "/bin/", root + "/icons/")):
                continue
            if name.startswith(root + "/"):
                relative = name[len(root) + 1:]
                if "/" in relative or relative not in metadata:
                    bad.append(f"unlisted runtime member: {name}")
            else:
                bad.append(f"unexpected top-level member: {name}")
        if bad:
            raise ValueError("rejected archive:\n  " + "\n  ".join(sorted(set(bad))))
        files = {member.name for member in members if member.isfile()}
        if not files & {root + "/bin/wptsall-client", root + "/bin/wptsall-client.exe"}:
            raise ValueError("missing product binary")
        if ui + "index.html" not in files:
            raise ValueError("missing UI entrypoint")
        try:
            version = archive.extractfile(root + "/VERSION-BINARY").read().decode("utf-8").strip()
        except KeyError:
            version = ""
        if expected_version and version != expected_version:
            raise ValueError(f"version mismatch: expected {expected_version}, got {version!r}")
    return {"status": "passed", "product": product, "kit": str(kit),
            "members": len(members), "binary_version": version}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("product", choices=["webui", "desktop"])
    parser.add_argument("kit", type=Path)
    parser.add_argument("--version", default="")
    args = parser.parse_args()
    print(json.dumps(validate(args.product, args.kit, args.version)))


if __name__ == "__main__":
    main()
