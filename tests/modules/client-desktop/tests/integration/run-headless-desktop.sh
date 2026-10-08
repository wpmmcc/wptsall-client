#!/usr/bin/env bash
# Headless Tauri command integration test suite (no Xvfb / WebKit required).
set -euo pipefail

_SD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
while [[ "$_SD" != "/" && ! -f "$_SD/scripts/wptsall.sh" ]]; do _SD="$(dirname "$_SD")"; done
ROOT_DIR="$_SD"

if ! command -v cargo >/dev/null 2>&1 && [ -f "${HOME}/.cargo/env" ]; then
  # shellcheck source=/dev/null
  source "${HOME}/.cargo/env"
fi

source "${ROOT_DIR}/tests/scripts/private-overlay.sh"
private_test_overlay "$ROOT_DIR"
MANIFEST="${WPTSALL_PRIVATE_OVERLAY}/client-desktop/src-tauri/Cargo.toml"

if [ ! -f "${MANIFEST}" ]; then
  echo "❌ [test:desktop-integration] manifest missing: ${MANIFEST}" >&2
  exit 2
fi

# Target dir, incremental and sccache policy are shared with every other
# compile entry point (scripts/lib/cargo-cache.sh).
# shellcheck source=/dev/null
source "${ROOT_DIR}/scripts/lib/cargo-cache.sh"
wptsall_setup_cargo_cache "desktop-integration" tests "${ROOT_DIR}/client-desktop/src-tauri"
cargo generate-lockfile --offline --manifest-path "${MANIFEST}"

echo "[test:desktop-integration] Running headless Tauri command integration suite..."
REPORT_DIR="${ROOT_DIR}/tests/reports/client-desktop"
mkdir -p "${REPORT_DIR}"
# Report-schema contract (guards report-schema-fresh-reports): execution
# reports carry dry_run=false + evidence paths; cargo output goes to the
# sibling log so the evidence pair (json + log) exists and stays fresh.
cargo test --offline --locked --manifest-path "${MANIFEST}" --test headless_commands "$@" \
  2>&1 | tee "${REPORT_DIR}/desktop-integration-latest.log"
CARGO_EXIT="${PIPESTATUS[0]}"
if [ "${CARGO_EXIT}" -ne 0 ]; then
  echo "❌ [test:desktop-integration] cargo test failed (exit ${CARGO_EXIT})" >&2
  exit 1
fi

cat <<EOF > "${REPORT_DIR}/desktop-integration-latest.json"
{
  "test_id": "TEST-DESKTOP-INTEGRATION-001",
  "requirement_id": "REQ-DESKTOP-INTEGRATION-001",
  "scenario_id": "SCENARIO-DESKTOP-HEADLESS-INTEGRATION",
  "user_journey_id": "UJ7",
  "user_journey_ids": ["UJ7", "UJ8", "UJ9"],
  "status": "passed",
  "dry_run": false,
  "mode": "headless_no_display",
  "timestamp": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "evidence": [
    "${REPORT_DIR}/desktop-integration-latest.json",
    "${REPORT_DIR}/desktop-integration-latest.log"
  ]
}
EOF

echo "✅ [test:desktop-integration] passed"
