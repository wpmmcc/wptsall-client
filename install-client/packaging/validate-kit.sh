#!/usr/bin/env bash
# Keep the existing CLI; all entrypoints share the platform-independent validator.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$HERE/validate-kit.py" "$@"
