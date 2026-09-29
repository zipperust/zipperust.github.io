#!/usr/bin/env bash
# Point git at the versioned hooks in .githooks/ (per-clone; run once).
#
#   scripts/install-hooks.sh
#
# Afterwards every `git commit` / `git push` in this clone runs the checks in
# .githooks/. Uninstall with: git config --unset core.hooksPath
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

chmod +x .githooks/pre-commit .githooks/pre-push 2>/dev/null || true
git config core.hooksPath .githooks

echo "==> core.hooksPath → $(git config --get core.hooksPath)"
echo "    active hooks: $(ls .githooks | tr '\n' ' ')"
