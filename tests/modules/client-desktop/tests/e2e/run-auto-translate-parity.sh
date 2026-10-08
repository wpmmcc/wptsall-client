#!/usr/bin/env bash
# Desktop auto-translate parity gate (no Tauri window required).
#
# Validates that shared WebUI auto-translate API paths are routed through
# proxy_webui_request on Desktop (discovery-tasks, worker/config review_mode,
# components/local, sync-pairs, batch-approve). Pair with:
#   bash tests/modules/wpmmcc-ats/e2e/run-simulation-lane.sh
# for the real dual-plugin WebUI journeys (SIM-06..08).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# e2e → tests → client-desktop → modules → tests → repo root
ROOT_DIR="$(cd "${SCRIPT_DIR}/../../../../.." && pwd)"
FRONTEND_DIR="${ROOT_DIR}/client-desktop/frontend"

cd "${FRONTEND_DIR}"
if command -v pnpm >/dev/null 2>&1; then
  pnpm test -- src/lib/api/auto-translate-routes.test.ts src/lib/api/client.test.ts src/lib/api/p1g2-routes.test.ts
else
  npm test -- src/lib/api/auto-translate-routes.test.ts src/lib/api/client.test.ts src/lib/api/p1g2-routes.test.ts
fi

echo "✅ desktop auto-translate API parity tests passed"
