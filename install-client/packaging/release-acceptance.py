#!/usr/bin/env python3
"""Validate an operator-signed private acceptance document after signature verification."""
from __future__ import annotations

import argparse
import datetime as dt
import json
from pathlib import Path
import re

PLATFORMS = {f"{os_}-{arch}" for os_ in ("linux", "windows", "darwin")
             for arch in ("x86_64", "aarch64")}
REQUIRED = {
    "core-private", "dual-plugin-native-http-multisite", "five-modalities-writers",
    "paid-receipt-recovery-all-finalizers", "reciprocal-relay-seo",
} | {f"native-install-ota-uninstall:{product}:{platform}"
     for product in ("webui", "desktop") for platform in PLATFORMS}


def validate(document: dict, repository: str, sha: str, version: str,
             now: dt.datetime | None = None) -> None:
    now = now or dt.datetime.now(dt.timezone.utc)
    if (document.get("format") != "private-release-acceptance-v1"
            or document.get("repository") != repository
            or document.get("source_sha") != sha
            or document.get("version") != version
            or document.get("status") != "passed"
            or not re.fullmatch(r"[0-9a-f]{40}", sha)
            or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version)):
        raise ValueError("acceptance does not bind this repository, commit and version")
    issued = dt.datetime.fromisoformat(document["issued_at"].replace("Z", "+00:00"))
    expires = dt.datetime.fromisoformat(document["expires_at"].replace("Z", "+00:00"))
    if (issued.utcoffset() != dt.timedelta(0) or expires.utcoffset() != dt.timedelta(0)
            or not issued <= now < expires
            or expires - issued > dt.timedelta(days=7)):
        raise ValueError("acceptance is expired, future-dated or too long-lived")
    checks = document.get("checks", {})
    for name in REQUIRED:
        check = checks.get(name, {})
        if (check.get("status") != "passed"
                or not re.fullmatch(r"[0-9a-f]{64}", check.get("evidence_sha256", ""))):
            raise ValueError(f"missing exact full-scope private acceptance: {name}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("document", type=Path)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--sha", required=True)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    validate(json.loads(args.document.read_bytes()), args.repository, args.sha, args.version)
    print("Full-scope private acceptance binds this exact candidate.")


if __name__ == "__main__":
    main()
