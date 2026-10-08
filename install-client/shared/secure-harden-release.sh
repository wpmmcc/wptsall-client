#!/usr/bin/env bash
# Post-build security hardening for release packages.
#
# Does:
#   - Strip symbols from ELF/PE/Mach-O product binaries
#   - Remove accidental source maps / .pdb / .dSYM
#   - Secret-pattern scan of staged text/config
#   - SHA256SUMS for all shipped binaries
#   - Optional code-sign hooks via env (no-op if secrets absent)
#
# Does NOT:
#   - UPX / commercial packers (high AV false-positive rate)
#   - String encryption that breaks stack traces
#
# Usage:
#   bash scripts/secure-harden-release.sh /path/to/stage
set -euo pipefail

STAGE="${1:-${STAGE:-}}"
if [ -z "$STAGE" ] || [ ! -d "$STAGE" ]; then
  echo "usage: $0 <stage-dir>" >&2
  exit 2
fi
STAGE="$(cd "$STAGE" && pwd)"
BIN="$STAGE/bin"
REPORT="$STAGE/SECURE-HARDEN-REPORT.txt"

log() { echo "[secure-harden] $*"; }
warn() { echo "[secure-harden] WARN: $*" >&2; }

: > "$REPORT"
echo "secure-harden report $(date -Iseconds 2>/dev/null || date '+%Y-%m-%dT%H:%M:%S%z')" >> "$REPORT"
echo "stage=$STAGE" >> "$REPORT"

# ── 1. Find product binaries ─────────────────────────────────────────────────
BINS=()
if [ -d "$BIN" ]; then
  while IFS= read -r _f; do
    [ -n "$_f" ] && BINS+=("$_f")
  done < <(find "$BIN" -maxdepth 1 -type f \( -name 'wptsall*' -o -name '*.exe' \) 2>/dev/null | sort)
fi

# ── 2. Strip symbols ─────────────────────────────────────────────────────────
strip_one() {
  local f="$1"
  local before after
  before=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f" 2>/dev/null || echo 0)

  if command -v llvm-strip >/dev/null 2>&1; then
    llvm-strip --strip-all "$f" 2>/dev/null || true
  elif command -v strip >/dev/null 2>&1; then
    strip "$f" 2>/dev/null || true
  fi

  # Cross-platform strip for PE binaries
  if file "$f" 2>/dev/null | grep -qi "PE32"; then
    if command -v x86_64-w64-mingw32-strip >/dev/null 2>&1; then
      x86_64-w64-mingw32-strip "$f" 2>/dev/null || true
    fi
  fi

  after=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f" 2>/dev/null || echo 0)
  log "strip $f: ${before} → ${after} bytes"
  echo "strip $(basename "$f"): ${before} → ${after}" >> "$REPORT"
}

for b in "${BINS[@]}"; do
  strip_one "$b"
done

# ── 3. Remove dev artifacts ───────────────────────────────────────────────────
log "Removing dev artifacts..."
REMOVED=0
while IFS= read -r artifact; do
  [ -n "$artifact" ] && rm -rf "$artifact" && REMOVED=$((REMOVED + 1))
done < <(find "$STAGE" \( \
  -name '*.pdb' -o -name '*.dSYM' -o -name '*.map' -o \
  -name '*.d.ts' -o -name '*.rs' -o -name 'Cargo.toml' -o \
  -name 'Cargo.lock' -o -name '.git' -o -name '.gitignore' -o \
  -name 'node_modules' -o -name 'src' -o -name '*.o' -o -name '*.a' \
  \) 2>/dev/null)
echo "removed_artifacts=$REMOVED" >> "$REPORT"
log "Removed $REMOVED dev artifacts"

# ── 4. Secret pattern scan ────────────────────────────────────────────────────
log "Scanning for leaked secrets..."
SECRET_PATTERNS=(
  'PRIVATE[_ ]KEY'
  'BEGIN RSA'
  'BEGIN EC PRIVATE'
  'sk-[a-zA-Z0-9]{20,}'
  'ghp_[a-zA-Z0-9]{36}'
  'gho_[a-zA-Z0-9]{36}'
  'AKIA[0-9A-Z]{16}'
  'password\s*[:=]\s*["\x27][^"\x27]{4,}'
)

SECRETS_FOUND=0
for pattern in "${SECRET_PATTERNS[@]}"; do
  if grep -rIl "$pattern" "$STAGE" 2>/dev/null | grep -v "SECURE-HARDEN-REPORT" | head -3 | grep -q .; then
    warn "Potential secret pattern found: $pattern"
    SECRETS_FOUND=$((SECRETS_FOUND + 1))
  fi
done

if [ "$SECRETS_FOUND" -gt 0 ]; then
  echo "SECRET_SCAN=FAIL (${SECRETS_FOUND} patterns)" >> "$REPORT"
  warn "Secret scan found $SECRETS_FOUND potential leaks — review before shipping!"
  if [ "${WPTSALL_HARDEN_STRICT:-0}" = "1" ]; then
    echo "ERROR: strict hardening rejects secret-like content" >&2
    exit 1
  fi
else
  echo "SECRET_SCAN=PASS" >> "$REPORT"
  log "Secret scan: PASS"
fi

# ── 5. Source code leak check ─────────────────────────────────────────────────
log "Checking for source code leakage..."
SRC_LEAK=0
if find "$STAGE" -name "*.rs" -o -name "*.ts" -o -name "*.svelte" | grep -q .; then
  warn "Source code files found in stage!"
  SRC_LEAK=1
fi
if find "$STAGE" -name "Cargo.toml" | grep -q .; then
  warn "Cargo.toml found in stage!"
  SRC_LEAK=1
fi
echo "SOURCE_LEAK_CHECK=$([ $SRC_LEAK -eq 0 ] && echo 'PASS' || echo 'FAIL')" >> "$REPORT"
if [ "$SRC_LEAK" -ne 0 ] && [ "${WPTSALL_HARDEN_STRICT:-0}" = "1" ]; then
  echo "ERROR: strict hardening rejects source-like files in stage" >&2
  exit 1
fi

# ── 6. SHA256SUMS ─────────────────────────────────────────────────────────────
log "Generating SHA256SUMS..."
SUMS_FILE="$STAGE/SHA256SUMS"
: > "$SUMS_FILE"
while IFS= read -r f; do
  [ -n "$f" ] || continue
  rel="${f#$STAGE/}"
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$STAGE" && sha256sum "$rel") >> "$SUMS_FILE"
  elif command -v shasum >/dev/null 2>&1; then
    (cd "$STAGE" && shasum -a 256 "$rel") >> "$SUMS_FILE"
  fi
done < <(find "$STAGE/bin" -type f 2>/dev/null; find "$STAGE/ui" -name "index.html" 2>/dev/null)

echo "sha256sums_count=$(wc -l < "$SUMS_FILE")" >> "$REPORT"
log "SHA256SUMS: $(wc -l < "$SUMS_FILE") entries"

# ── 7. Code signing hook (no-op without secrets) ─────────────────────────────
if [ -n "${WPTSALL_CODESIGN_HOOK:-}" ] && [ -x "${WPTSALL_CODESIGN_HOOK}" ]; then
  log "Running code-sign hook..."
  bash "${WPTSALL_CODESIGN_HOOK}" "$STAGE"
  echo "CODESIGN=applied" >> "$REPORT"
else
  echo "CODESIGN=skipped (no hook)" >> "$REPORT"
fi

log "Hardening complete. Report: $REPORT"
