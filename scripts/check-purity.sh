#!/usr/bin/env bash
# BYOA purity gate — fail if bundled game content leaks into either the public
# compiled wasm or the git tree. Executable form of the AGENTS.md invariant.
#
#   ./scripts/check-purity.sh [path/to/zipper_wasm_bg.wasm]
#
# Guards:
#   1. Known game-content strings must not appear in the compiled wasm. The
#      engine ships no dialog / names; the JS extractors read those from the
#      user's own Zipper.pdx at runtime.
#   2. Game assets / golden fixtures must never be tracked in git.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

WASM="${1:-pkg/zipper_wasm_bg.wasm}"
status=0

# --- 1. wasm canaries -------------------------------------------------------
if [[ -f "$WASM" ]]; then
  # Distinctive tokens from Zipper's own content. None may survive the build.
  PATTERNS='Henchmen|vegetables|You should not have returned|A game by Bennett Foddy'
  if ! command -v strings >/dev/null 2>&1; then
    echo "purity: 'strings' not found; cannot scan $WASM" >&2
    status=1
  else
    hits="$(strings -n 6 "$WASM" | grep -iE "$PATTERNS" || true)"
    if [[ -n "$hits" ]]; then
      echo "purity: game-content strings found in $WASM:" >&2
      printf '%s\n' "$hits" >&2
      status=1
    else
      echo "purity: wasm clean ($WASM)"
    fi
  fi
else
  echo "purity: skip wasm scan (no $WASM)" >&2
fi

# --- 2. git-tracked content -------------------------------------------------
# Only run where a git tree exists (a tarball build has no history to inspect).
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  FORBIDDEN='^(ref/|assets/|crates/zipper-core/data/.*\.(bin|json)$)|\.(pdx|pdz|pdi|pdt|pft|pda)$'
  tracked="$(git ls-files | grep -E "$FORBIDDEN" || true)"
  if [[ -n "$tracked" ]]; then
    echo "purity: game assets / fixtures are tracked in git:" >&2
    printf '%s\n' "$tracked" >&2
    status=1
  else
    echo "purity: git tree clean (no assets / fixtures tracked)"
  fi
else
  echo "purity: skip git scan (not a work tree)" >&2
fi

exit "$status"
