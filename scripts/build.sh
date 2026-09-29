#!/usr/bin/env bash
# Build the zipper-wasm engine into ./pkg for the static shell at the repo root.
#
#   ./scripts/build.sh            # native parity tests + wasm build (auto-bumps letter)
#   GOD=1 ./scripts/build.sh      # include the dev God tools (hidden unless #god)
#   SKIP_TESTS=1 ./scripts/build.sh   # CI: skip parity tests (no local Zipper.pdx)
#   NO_BUMP=1 ./scripts/build.sh      # don't advance PORT_VERSION (use committed stamp)
#   NO_STAMP=1 ./scripts/build.sh     # build/verify but don't rewrite tracked shell files
#   CACHE_TOKEN=<sha> ./scripts/build.sh  # cache-bust ?v=/SW with e.g. a commit SHA
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VER_FILE="$ROOT/crates/zipper-core/PORT_VERSION"

# Letter advances every compile; bump minor (+ reset letter to a) by hand for features.
bump_port_letter() {
  local cur major minor letter next alphabet="abcdefghijklmnopqrstuvwxyz"
  cur="$(tr -d '[:space:]' < "$VER_FILE")"
  if [[ ! "$cur" =~ ^([0-9]+)\.([0-9]+)([a-z])$ ]]; then
    echo "bad PORT_VERSION: '$cur' (expected e.g. 0.2a)" >&2
    exit 1
  fi
  major="${BASH_REMATCH[1]}"
  minor="${BASH_REMATCH[2]}"
  letter="${BASH_REMATCH[3]}"
  if [[ "$letter" == "z" ]]; then
    echo "PORT_VERSION letter overflow at ${major}.${minor}z — bump minor and reset to a" >&2
    exit 1
  fi
  next="${alphabet#*"$letter"}"
  next="${next:0:1}"
  printf '%s.%s%s\n' "$major" "$minor" "$next" > "$VER_FILE"
  echo "==> port version → v$(tr -d '[:space:]' < "$VER_FILE")"
}

if [[ "${NO_BUMP:-}" == "1" ]]; then
  echo "==> NO_BUMP=1 — keeping committed port version v$(tr -d '[:space:]' < "$VER_FILE")"
else
  bump_port_letter
fi

# --- Native parity tests (skipped in CI, where no Zipper.pdx/golden data exists) ---
if [[ "${SKIP_TESTS:-}" == "1" ]]; then
  echo "==> SKIP_TESTS=1 — skipping zipper-core parity tests"
else
  DATA="$ROOT/crates/zipper-core/data/worldmap.bin"
  if [[ ! -f "$DATA" ]]; then
    cat >&2 <<EOF
error: missing local game data: crates/zipper-core/data/worldmap.bin

The parity test suite needs the golden fixtures derived from your own Zipper.pdx
(worldmap.bin + dialogs.json + introchord.json). They are gitignored on purpose:
this repo ships no game content.

To run the tests:
  1. Put a Zipper.pdx copy at ref/Zipper.pdx (Data Disk / sideload build).
  2. Generate the fixtures with the offline extract tooling, then re-run
     ./scripts/build.sh.

CI / data-free machines: SKIP_TESTS=1 ./scripts/build.sh
EOF
    exit 1
  fi

  # Core tests must run as a native host binary. A machine-wide
  # `CARGO_BUILD_TARGET=wasm32-unknown-unknown` (or cargo config `build.target`)
  # would otherwise produce a `.wasm` the host cannot exec.
  HOST_TRIPLE="$(rustc -vV | awk '/^host:/{print $2; found=1} END{exit !found}')"
  if [[ -z "$HOST_TRIPLE" ]]; then
    echo "error: could not read host triple from rustc -vV" >&2
    exit 1
  fi
  echo "==> zipper-core tests (host ${HOST_TRIPLE})"
  unset CARGO_BUILD_TARGET || true
  echo "    \$ cargo test -p zipper-core --target ${HOST_TRIPLE}"
  cargo test -p zipper-core --target "$HOST_TRIPLE"
fi

# --- wasm build → ./pkg ---
echo "==> wasm-pack (web target → ./pkg)"
if [[ "${GOD:-}" == "1" ]]; then
  echo "==> god tools ENABLED (GOD=1)"
  wasm-pack build crates/zipper-wasm \
    --target web \
    --out-dir "$ROOT/pkg" \
    --out-name zipper_wasm \
    --release \
    --features god
else
  echo "==> god tools disabled (set GOD=1 to enable)"
  wasm-pack build crates/zipper-wasm \
    --target web \
    --out-dir "$ROOT/pkg" \
    --out-name zipper_wasm \
    --release
fi

# Drop npm noise we don't need for a static shell.
rm -f "$ROOT/pkg/.gitignore" \
      "$ROOT/pkg/package.json" \
      "$ROOT/pkg/README.md"

# --- BYOA purity gate: no game content in the wasm / git tree ---
"$ROOT/scripts/check-purity.sh" "$ROOT/pkg/zipper_wasm_bg.wasm"

# --- Stamp shell so phones don't keep a stale cached build ---
VER="$(tr -d '[:space:]' < "$VER_FILE")"
# Cache-busting token: commit SHA in CI, semantic stamp locally.
TOKEN="${CACHE_TOKEN:-$VER}"

printf '%s\n' "$VER" > "$ROOT/PORT_VERSION"
echo "==> wrote PORT_VERSION → ${VER} (cache token: ${TOKEN})"

if [[ "${NO_STAMP:-}" == "1" ]]; then
  echo "==> NO_STAMP=1 — leaving tracked shell files untouched"
else
if [[ -f "$ROOT/index.html" ]]; then
  python3 - "$ROOT/index.html" "$TOKEN" <<'PY'
import pathlib, re, sys
path = pathlib.Path(sys.argv[1])
token = sys.argv[2]
text = path.read_text()
text2 = re.sub(
    r'(href|src)="(style\.css|main\.js)(\?v=[^"]*)?"',
    rf'\1="\2?v={token}"',
    text,
)
if text2 != text:
    path.write_text(text2)
    print(f"==> stamped index.html shell assets → ?v={token}")
PY
fi

if [[ -f "$ROOT/main.js" ]]; then
  python3 - "$ROOT/main.js" "$TOKEN" <<'PY'
import pathlib, re, sys
path = pathlib.Path(sys.argv[1])
token = sys.argv[2]
text = path.read_text()
text2, n = re.subn(
    r'(from\s+")(\./pkg/zipper_wasm\.js)(\?v=[^"]*)?(")',
    rf'\1\2?v={token}\4',
    text,
    count=1,
)
if n:
    path.write_text(text2)
    print(f"==> stamped main.js pkg import → ?v={token}")
else:
    print("warn: could not stamp pkg import in main.js", file=sys.stderr)
PY
fi

# Stamp the service-worker cache name so rebuilds replace the offline cache.
if [[ -f "$ROOT/sw.js" ]]; then
  python3 - "$ROOT/sw.js" "$TOKEN" <<'PY'
import pathlib, re, sys
path = pathlib.Path(sys.argv[1])
token = sys.argv[2]
text = path.read_text()
text2, n = re.subn(
    r"(const CACHE = ')zipper-rust-v[^']*(')",
    rf"\1zipper-rust-v{token}\2",
    text,
    count=1,
)
if n:
    path.write_text(text2)
    print(f"==> stamped sw.js CACHE → zipper-rust-v{token}")
else:
    print("warn: could not stamp CACHE in sw.js", file=sys.stderr)
PY
fi
fi

echo "==> done. Serve the repo root (e.g. python3 -m http.server 8080). Stamp v${VER}"
