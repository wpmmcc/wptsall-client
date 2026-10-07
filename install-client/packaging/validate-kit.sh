#!/usr/bin/env bash
# Validate a source-free client kit after it has been packed.
#
# This check is deliberately independent of filename globs.  It inspects every
# tar member, rejects unsafe/source-like members and verifies the product root,
# binary, UI and optional binary version.  It is used before signing/publishing
# and may also be run by public pack-and-test runners.
set -euo pipefail

PRODUCT="${1:-}"
KIT="${2:-}"
EXPECTED_VERSION=""
shift 2 || true
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) EXPECTED_VERSION="${2:-}"; shift 2 ;;
    -h|--help)
      echo "Usage: $0 webui|desktop KIT [--version VERSION]"
      exit 0
      ;;
    *) echo "[validate-kit] unknown option: $1" >&2; exit 2 ;;
  esac
done

[[ "$PRODUCT" == "webui" || "$PRODUCT" == "desktop" ]] || { echo "[validate-kit] product must be webui or desktop" >&2; exit 2; }
[[ -f "$KIT" ]] || { echo "[validate-kit] kit not found: $KIT" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "[validate-kit] python3 is required" >&2; exit 1; }

python3 - "$PRODUCT" "$KIT" "$EXPECTED_VERSION" <<'PY'
import json
import pathlib
import sys
import tarfile

product, kit_path, expected_version = sys.argv[1:]
kit = pathlib.Path(kit_path)
root_name = "wptsall-client-webui" if product == "webui" else "wptsall-client"
ui_prefix = f"{root_name}/ui/{'webui' if product == 'webui' else 'desktop'}/"
bin_name = "wptsall-client"

allowed_meta = {
    "VERSION",
    "VERSION-BINARY",
    "VERSION-WEBUI",
    "PLATFORM.txt",
    "TRIPLE.txt",
    "HARDENING.txt",
    "OBFUSCATION.txt",
    "SECURE-HARDEN-REPORT.txt",
    "SHA256SUMS",
    # 发布批 P1: Gatekeeper/SmartScreen authorization instructions shipped
    # inside every kit (desktop builds carry no Apple/Microsoft OS cert).
    "FIRST-LAUNCH-AUTHORIZATION.md",
}
forbidden_suffixes = (
    ".rs", ".ts", ".svelte", ".map", ".pdb", ".dSYM", ".o", ".a",
)
forbidden_parts = {
    ".git", "node_modules", "target", "src", "src-tauri", "Cargo.toml",
    "Cargo.lock", "data", "runtime", "logs", "tests", "test-results",
}
forbidden_names = {".env", ".env.local", ".env.production", "id_rsa", "credentials.json"}

with tarfile.open(kit, "r:gz") as archive:
    members = archive.getmembers()
    if not members:
        raise SystemExit("[validate-kit] archive is empty")
    names = [m.name for m in members]
    bad = []
    for member in members:
        name = member.name
        normalized = pathlib.PurePosixPath(name)
        if name.startswith("/") or ".." in normalized.parts:
            bad.append(f"unsafe path: {name}")
        if member.issym() or member.islnk() or not (member.isfile() or member.isdir()):
            bad.append(f"unsupported tar member type: {name}")
        parts = set(normalized.parts)
        if parts & forbidden_parts:
            bad.append(f"forbidden member: {name}")
        basename = normalized.name.lower()
        if basename in forbidden_names or basename.endswith((".pem", ".key", ".p12", ".pfx")):
            bad.append(f"credential/secret member: {name}")
        if name.endswith(forbidden_suffixes):
            bad.append(f"forbidden source/build suffix: {name}")

        if name == root_name or name == root_name + "/":
            continue
        if name.rstrip("/") in {
            f"{root_name}/bin",
            f"{root_name}/ui",
            f"{root_name}/icons",
            ui_prefix.rstrip("/"),
        }:
            continue
        if name.startswith(ui_prefix):
            continue
        if name.startswith(root_name + "/bin/"):
            continue
        if name.startswith(root_name + "/icons/"):
            continue
        if name.startswith(root_name + "/"):
            rel = name[len(root_name) + 1:]
            if "/" in rel:
                # Only bin/*, icons/* and the selected ui/* tree are runtime content.
                if not rel.startswith("bin/") and not rel.startswith("icons/"):
                    bad.append(f"unlisted runtime member: {name}")
            elif rel not in allowed_meta:
                bad.append(f"unlisted root member: {name}")
        elif name.rstrip("/"):
            bad.append(f"unexpected top-level member: {name}")

    if bad:
        raise SystemExit("[validate-kit] rejected archive:\n  " + "\n  ".join(sorted(set(bad))))

    root_files = {m.name for m in members if m.isfile()}
    binary_candidates = {f"{root_name}/bin/{bin_name}", f"{root_name}/bin/{bin_name}.exe"}
    if not root_files & binary_candidates:
        raise SystemExit(f"[validate-kit] missing {bin_name} binary under {root_name}/bin")
    ui_index = ui_prefix + "index.html"
    if ui_index not in root_files:
        raise SystemExit(f"[validate-kit] missing UI entrypoint: {ui_index}")

    def read_member(relative):
        name = f"{root_name}/{relative}"
        try:
            return archive.extractfile(name).read().decode("utf-8", errors="strict").strip()
        except KeyError:
            return ""

    binary_version = read_member("VERSION-BINARY")
    if expected_version and binary_version != expected_version:
        raise SystemExit(
            f"[validate-kit] version mismatch: expected {expected_version}, got {binary_version or '<missing>'}"
        )

print(json.dumps({
    "status": "passed",
    "product": product,
    "kit": str(kit),
    "members": len(members),
    "binary_version": binary_version,
}, ensure_ascii=False))
PY
