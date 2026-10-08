#!/usr/bin/env python3
"""Private, isolated log-redaction mutation probes, never edit product source."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[4]
SOURCE = ROOT / "client-wpplugin/source/src/logging.rs"
DEPS = ROOT / "client-wpplugin/source/target/debug/deps"
HARNESS = r'''
#[path = "logging.rs"]
mod logging;
fn main() {
    let credential = "owned-mutation-marker\\\"tail";
    let inner = serde_json::json!({"nested": [{"key": credential}]}).to_string();
    let encoded = serde_json::to_string(&inner).unwrap();
    for input in [format!("response={inner}"), format!("response={encoded}")] {
        let output = logging::redact_string_for_log(&input);
        assert!(!output.contains("owned-mutation-marker"), "encoded JSON leaked");
        assert!(output.contains("[REDACTED]"), "encoded JSON lacks redaction");
    }
    for suffix in ["/sync/push", "/sync", "/sync?format=text"] {
        let route = format!("http://127.0.0.1/wp-json/wpmmcc/v1/owned-route-marker{suffix}");
        let output = logging::redact_string_for_log(&route);
        assert!(!output.contains("owned-route-marker"), "peer route leaked");
        assert!(output.ends_with(suffix), "safe route diagnostic lost");
    }
    assert_eq!(logging::redact_string_for_log("key count=3; object_id=42"),
               "key count=3; object_id=42");
    println!("OWNED LOGGING PROBE PASS");
}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument("--report", required=True, type=Path)
    args = parser.parse_args()
    args.evidence.mkdir(parents=True, exist_ok=True)
    original = SOURCE.read_text()
    original_hash = hashlib.sha256(SOURCE.read_bytes()).hexdigest()
    manifest = tomllib.loads((ROOT / "client-wpplugin/source/Cargo.toml").read_text())
    compile_env = dict(os.environ, CARGO_PKG_VERSION=manifest["package"]["version"])
    mutations = {
        "baseline": None,
        "json-parser-disabled": (
            """if matches!(bytes[i], b'{' | b'[' | b'"') {""",
            """if false && matches!(bytes[i], b'{' | b'[' | b'"') {""",
        ),
        "peer-route-disabled": (
            'let peer_prefix = "/wpmmcc/v1/";',
            'let peer_prefix = "/owned-disabled-route/";',
        ),
    }
    extern = []
    for name in ["anyhow", "serde_json", "url"]:
        candidates = list(DEPS.glob(f"lib{name}-*.rlib"))
        if not candidates:
            raise RuntimeError(f"warm debug dependency absent: {name}")
        dependency = max(candidates, key=lambda p: p.stat().st_mtime_ns)
        extern += ["--extern", f"{name}={dependency}"]
    rows = []
    for name, mutation in mutations.items():
        path = args.evidence / name
        path.mkdir(exist_ok=True)
        source = original
        if mutation:
            old, new = mutation
            if source.count(old) != 1:
                raise RuntimeError(f"mutation is no longer applicable: {name}")
            source = source.replace(old, new, 1)
        (path / "logging.rs").write_text(source)
        (path / "main.rs").write_text(HARNESS)
        with (path / "build.log").open("w") as log:
            built = subprocess.run(
                ["rustc", "--edition=2021", "--crate-name", "owned_logging_probe",
                 str(path / "main.rs"), "-L", f"dependency={DEPS}",
                 *extern, "-o", str(path / "probe")],
                stdout=log, stderr=subprocess.STDOUT, timeout=60, env=compile_env,
            )
        if built.returncode:
            raise RuntimeError(f"mutation compile failure is not a kill: {name}")
        with (path / "probe.log").open("w") as log:
            result = subprocess.run(
                [str(path / "probe")], stdout=log, stderr=subprocess.STDOUT, timeout=15
            )
        valid = result.returncode == 0 if mutation is None else result.returncode != 0
        rows.append({"name": name, "compile_exit": built.returncode,
                     "probe_exit": result.returncode, "expected_outcome": valid,
                     "source_sha256": hashlib.sha256(source.encode()).hexdigest()})
    unchanged = hashlib.sha256(SOURCE.read_bytes()).hexdigest() == original_hash
    report = {"kind": "isolated-logging-mutations", "production_source_unchanged": unchanged,
              "checks": rows, "all_passed": unchanged and all(r["expected_outcome"] for r in rows)}
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    print("Owned logging mutation outcomes:", [(r["name"], r["probe_exit"]) for r in rows])
    return 0 if report["all_passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
