#!/usr/bin/env bash
# Client unit gate for `scripts/wptsall.sh test client-unit`.
#
# Execution plan 2-2 (ISS A-1 / NEW-T-03): every [[test]] target declared in
# Cargo.toml is enumerated and executed here. Declared-but-not-run is a hard
# FAIL — the two targets that used to be silently skipped
# (wp_core_source_component_flow, wp_translation_providers_roundtrip) now run
# on every invocation. sign_e2e stays conditional on the mock translate API
# (127.0.0.1:9090); when the mock is unreachable the target is recorded as
# environment_missing and the gate exits 2 (not green, not failed) — never a
# silent pass.
#
# WPTSALL_CLIENT_UNIT_SKIP_TARGETS="a,b" deliberately leaves declared targets
# unexecuted, which FAILS the gate. It exists to verify the fail-closed rule
# (execution plan 2-2 acceptance) and serves no other purpose.
#
# catalog-contract: cargo-test-targets — this runner enumerates and executes
# every [[test]] target declared in Cargo.toml (fail-closed: declared-not-run
# FAILs). catalog_core.extract_cargo_targets() treats this marker plus the
# variable `--test "$..."` loop as runner coverage for ALL declared targets.
#
# Summary JSON: tests/reports/client-unit/unit-<ts>.json (report contract).
#
# Exit codes: 0 = every declared target executed and passed
#             1 = any failure, or any declared target not executed (fail-closed)
#             2 = environment_missing only (e.g. mock down for sign_e2e)
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
mkdir -p "${ROOT_DIR}/tests/.cache"
OVERLAY="$(mktemp -d "${ROOT_DIR}/tests/.cache/client-unit-overlay.XXXXXX")"
python3 "${ROOT_DIR}/scripts/private-source-layout.py" overlay --out "$OVERLAY" || exit 1
SOURCE_DIR="${OVERLAY}/client-wpplugin/source"
FRONTEND_DIR="${SOURCE_DIR}/frontend"
# Private fixtures must never read the caller's data or encryption authority.
export WPTSALL_DATA_DIR="${OVERLAY}/owned-runtime"
export WPTSALL_COMPONENT_BINDINGS_SECRET="private-fixture-bindings-key-not-for-production"
export WPTSALL_PROXY_PROFILES_FILE="${OVERLAY}/owned-runtime/proxy-profiles.json"
source "${ROOT_DIR}/scripts/lib/cargo-cache.sh"
wptsall_setup_cargo_cache "wpplugin-unit" tests "${ROOT_DIR}/client-wpplugin/source"

REPORTS_DIR="${ROOT_DIR}/tests/reports/client-unit"
mkdir -p "${REPORTS_DIR}"
STAMP="$(date +%Y%m%d-%H%M%S)"
SUMMARY_JSON="${REPORTS_DIR}/unit-${STAMP}.json"
LOG="${REPORTS_DIR}/unit-${STAMP}.log"
RESULTS="$(mktemp)"
: > "${RESULTS}"

log() { printf '%s\n' "$*" | tee -a "${LOG}" >&2; }

ensure_toolchain_paths() {
  if ! command -v cargo >/dev/null 2>&1 && [ -f "${HOME}/.cargo/env" ]; then
    # shellcheck source=/dev/null
    source "${HOME}/.cargo/env"
  fi
  command -v cargo >/dev/null 2>&1 || { echo "❌ [client-tests] cargo not available" >&2; exit 2; }
}

ensure_npm_deps() {
  local app_dir="$1"
  local lock_file="${app_dir}/package-lock.json"
  local node_lock_file="${app_dir}/node_modules/.package-lock.json"
  if [ ! -d "${app_dir}/node_modules" ] || \
     { [ -f "${lock_file}" ] && { [ ! -f "${node_lock_file}" ] || [ "${lock_file}" -nt "${node_lock_file}" ]; }; }; then
    echo "[client-tests] installing npm dependencies in ${app_dir}"
    (cd "${app_dir}" && npm ci)
  fi
}

cargo_counts() {
  # Sum cargo "test result: ok. N passed; M failed;" lines from stdin.
  awk '
    /^test result:/ {
      for (i = 2; i <= NF; i++) {
        if ($i ~ /^passed;?$/) p += $(i - 1)
        if ($i ~ /^failed;?$/) f += $(i - 1)
      }
    }
    END { printf "%d %d\n", p + 0, f + 0 }
  '
}

record() { # name kind status passed failed
  printf '%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" "$5" >> "${RESULTS}"
}

run_cargo_group() { # label kind cargo-args...
  local name="$1" kind="$2"
  shift 2
  local out rc p f
  log "[client-tests] running ${kind}: ${name}"
  out="$(cargo test --locked --offline --manifest-path "${SOURCE_DIR}/Cargo.toml" "$@" 2>&1)"
  rc=$?
  printf '%s\n' "${out}" >> "${LOG}"
  read -r p f <<< "$(printf '%s\n' "${out}" | cargo_counts)"
  if [[ ${rc} -eq 0 ]]; then
    record "${name}" "${kind}" "passed" "${p}" "${f}"
    log "✅ ${name} (${p} passed, ${f} failed)"
  else
    record "${name}" "${kind}" "failed" "${p}" "${f}"
    log "❌ ${name} (exit=${rc}, ${p} passed, ${f} failed) — tail:"
    printf '%s\n' "${out}" | tail -25 | sed 's/^/      /' | tee -a "${LOG}" >&2
  fi
  return 0
}

ensure_toolchain_paths
export CARGO_NET_OFFLINE="${CARGO_NET_OFFLINE:-true}"
# Resolve private-only dev dependencies in this owned stage, never production.
cargo generate-lockfile --manifest-path "${SOURCE_DIR}/Cargo.toml" --offline || exit 2
log "[client-tests] frozen private overlay: ${OVERLAY}"

# --- enumerate every declared [[test]] target (Cargo.toml is the SSOT) -------
mapfile -t DECLARED < <(python3 - "${SOURCE_DIR}/Cargo.toml" <<'PY'
import re
import sys

text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
names = []
for m in re.finditer(r"\[\[test\]\]\s*([^\[]*)", text):
    n = re.search(r'name\s*=\s*"([^"]+)"', m.group(1))
    if n:
        names.append(n.group(1))
print("\n".join(names))
PY
)
if [[ ${#DECLARED[@]} -lt 7 ]]; then
  log "❌ [client-tests] fewer than 7 [[test]] targets in Cargo.toml (got ${#DECLARED[@]})"
  exit 1
fi
log "[client-tests] declared [[test]] targets (${#DECLARED[@]}): ${DECLARED[*]}"

# --- in-crate unit tests ------------------------------------------------------
run_cargo_group "lib-bins" "in-crate" --lib --bins

# --- declared integration targets (fail-closed: none may stay unexecuted) ----
MOCK_UP=0
if curl -fsS --max-time 2 "http://127.0.0.1:9090/api/v1/health" >/dev/null 2>&1; then
  MOCK_UP=1
fi

SKIP_LIST="${WPTSALL_CLIENT_UNIT_SKIP_TARGETS:-}"
declare -A SKIPPED=()
if [[ -n "${SKIP_LIST}" ]]; then
  for t in ${SKIP_LIST//,/ }; do SKIPPED["${t}"]=1; done
fi

for target in "${DECLARED[@]}"; do
  if [[ -n "${SKIPPED[${target}]:-}" ]]; then
    record "${target}" "cargo-test" "not_run" 0 0
    log "❌ ${target} NOT RUN (WPTSALL_CLIENT_UNIT_SKIP_TARGETS) — declared targets must run"
    continue
  fi
  if [[ "${target}" == "sign_e2e" && "${MOCK_UP}" -ne 1 ]]; then
    record "${target}" "cargo-test" "environment_missing" 0 0
    log "⚠️  sign_e2e environment_missing: mock-translate-api not reachable on 127.0.0.1:9090 (recorded, exit will be 2 — not a silent pass)"
    continue
  fi
  run_cargo_group "${target}" "cargo-test" --test "${target}"
done

# --- frontend svelte-check + vitest -------------------------------------------
log "[client-tests] ensuring frontend npm dependencies"
ensure_npm_deps "${FRONTEND_DIR}"

# Type gate first: svelte-check catches the reduced-type-redeclaration
# drift class that Vitest (untyped) cannot (2026-09-12: 3 such errors
# shipped while every CI lane was green because none ran svelte-check).
log "[client-tests] running frontend svelte-check"
SC_OUT="$( (cd "${FRONTEND_DIR}" && npm run check) 2>&1 )"
SC_RC=$?
printf '%s\n' "${SC_OUT}" >> "${LOG}"
if [[ ${SC_RC} -eq 0 ]]; then
  record "frontend-svelte-check" "frontend" "passed" 0 0
  log "✅ frontend-svelte-check (0 errors)"
else
  record "frontend-svelte-check" "frontend" "failed" 0 0
  log "❌ frontend-svelte-check (exit=${SC_RC}) — tail:"
  printf '%s\n' "${SC_OUT}" | tail -25 | sed 's/^/      /' | tee -a "${LOG}" >&2
fi

log "[client-tests] running frontend unit tests"
FE_OUT="$( (cd "${FRONTEND_DIR}" && npm run test:unit) 2>&1 )"
FE_RC=$?
printf '%s\n' "${FE_OUT}" >> "${LOG}"
# vitest summary lines carry ANSI escapes ("Tests  123 passed"); strip them
# before counting so the summary JSON carries real numbers.
FE_PLAIN="$(printf '%s\n' "${FE_OUT}" | sed 's/\x1b\[[0-9;]*m//g')"
FE_P="$(printf '%s\n' "${FE_PLAIN}" | awk '/Tests[ \t]+[0-9]+ passed/ {for(i=1;i<=NF;i++) if ($(i+1) == "passed") s += $i} END {print s+0}')"
FE_F="$(printf '%s\n' "${FE_PLAIN}" | awk '/Tests[ \t]+[0-9]+ failed/ {for(i=1;i<=NF;i++) if ($(i+1) == "failed") s += $i} END {print s+0}')"
if [[ ${FE_RC} -eq 0 ]]; then
  record "frontend-vitest" "frontend" "passed" "${FE_P}" "${FE_F}"
  log "✅ frontend-vitest (${FE_P} passed, ${FE_F} failed)"
else
  record "frontend-vitest" "frontend" "failed" "${FE_P}" "${FE_F}"
  log "❌ frontend-vitest (exit=${FE_RC}) — tail:"
  printf '%s\n' "${FE_OUT}" | tail -25 | sed 's/^/      /' | tee -a "${LOG}" >&2
fi

# --- summary + exit -----------------------------------------------------------
EXIT_CODE="$(python3 - "${RESULTS}" "${SUMMARY_JSON}" "${LOG}" <<'PY'
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

results_file, summary_json, log_path = sys.argv[1:4]
targets = []
for line in open(results_file):
    name, kind, status, p, f = line.rstrip("\n").split("\t")
    targets.append({
        "name": name, "kind": kind, "status": status,
        "execution": "environment_missing" if status == "environment_missing"
                     else ("planned" if status == "not_run" else "actual"),
        "passed": int(p), "failed": int(f),
    })

any_failed = any(t["status"] in ("failed", "not_run") for t in targets)
any_env = any(t["status"] == "environment_missing" for t in targets)
overall = "failed" if any_failed else ("incomplete" if any_env else "passed")

phases = []
for t in targets:
    if t["status"] in ("failed", "not_run"):
        phase_status, exit_code = "failed", 1
    elif t["status"] == "environment_missing":
        phase_status, exit_code = "skipped", 2
    else:
        phase_status, exit_code = "passed", 0
    phases.append({
        "name": t["name"], "status": phase_status,
        "exit_code": exit_code, "required": True,
        "execution": t["execution"],
    })

payload = {
    "suite": "client-unit",
    "test_id": "TEST-CLIENT-UNIT-001",
    "status": overall,
    "dry_run": False,
    "generated": datetime.now(timezone.utc).astimezone().isoformat(timespec="seconds"),
    "summary": {
        "targets": len(targets),
        "passed": sum(1 for t in targets if t["status"] == "passed"),
        "failed": sum(1 for t in targets if t["status"] in ("failed", "not_run")),
        "environment_missing": sum(1 for t in targets if t["status"] == "environment_missing"),
        "tests_passed": sum(t["passed"] for t in targets),
        "tests_failed": sum(t["failed"] for t in targets),
    },
    "targets": targets,
    "phases": phases,
    "evidence": [summary_json, log_path],
}
Path(summary_json).write_text(
    json.dumps(payload, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
)
print(1 if any_failed else (2 if any_env else 0))
PY
)"
rm -f "${RESULTS}"

echo "[client-tests] summary -> ${SUMMARY_JSON} (status per exit ${EXIT_CODE})"
exit "${EXIT_CODE}"
