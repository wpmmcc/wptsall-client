#!/usr/bin/env bash
# Desktop lab-cases subset — same wizard-* testids as WebUI.
#
#   bash tests/modules/client-desktop/tests/e2e/run-lab-cases-subset.sh
#   LAB_CASES_LIMIT=5 LAB_CASE_FILTER=baidu,youdao bash tests/modules/client-desktop/tests/e2e/run-lab-cases-subset.sh
#
# Requires: mock :9090, desktop debug binary, tauri-driver, WebKitWebDriver, Xvfb.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export LAB_CASES_LIMIT="${LAB_CASES_LIMIT:-3}"
export LAB_INCLUDE_AUTH_PROFILES="${LAB_INCLUDE_AUTH_PROFILES:-0}"
export MOCK_API_BASE="${MOCK_API_BASE:-http://127.0.0.1:9090}"

echo "[desktop-lab-subset] LAB_CASES_LIMIT=${LAB_CASES_LIMIT} FILTER=${LAB_CASE_FILTER:-} MOCK=${MOCK_API_BASE}"
exec bash "${SCRIPT_DIR}/tauri-provider-wizard-from-lab-cases.sh"
