#!/usr/bin/env bash
# Derive the local golden fixtures from your own Zipper.pdx (BYOA).
#
# Nothing here is shipped: outputs land in gitignored paths (ref/, data/*.bin|json).
# Run once after cloning, or whenever you refresh your pdx copy.
#
#   scripts/gen-fixtures.sh
#
# Input:  ref/Zipper.pdx/main.pdz  (unzipped), or ref/Zipper.pdx.zip to unzip.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

PDX_DIR="ref/Zipper.pdx"
PDX_ZIP="ref/Zipper.pdx.zip"

if [[ ! -f "$PDX_DIR/main.pdz" ]]; then
  if [[ -f "$PDX_ZIP" ]]; then
    echo "==> unzipping $PDX_ZIP → $PDX_DIR/"
    python3 - "$PDX_ZIP" "$PDX_DIR" <<'PY'
import pathlib, sys, zipfile

zip_path, dest = sys.argv[1], pathlib.Path(sys.argv[2])
dest.mkdir(parents=True, exist_ok=True)
with zipfile.ZipFile(zip_path) as z:
    files = [
        n
        for n in z.namelist()
        if not n.endswith("/") and not n.startswith("__MACOSX/")
    ]
    prefix = "Zipper.pdx/" if any(n.startswith("Zipper.pdx/") for n in files) else ""
    for name in files:
        rel = name[len(prefix):] if prefix and name.startswith(prefix) else name
        if not rel:
            continue
        out = dest / rel
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_bytes(z.read(name))
PY
  else
    cat >&2 <<EOF
error: missing $PDX_DIR/main.pdz (and no $PDX_ZIP to unpack).

On a Playdate with Zipper installed, enable Data Disk, copy the Zipper.pdx
folder here as $PDX_ZIP, then re-run this script.
EOF
    exit 1
  fi
fi

[[ -f "$PDX_DIR/main.pdz" ]] || { echo "error: $PDX_DIR/main.pdz still missing" >&2; exit 1; }

echo "==> deriving worldmap.bin + dialogs.json (node)"
node scripts/gen-fixtures.mjs

echo "==> deriving introchord.json (rust)"
# Force the host target: a machine-wide CARGO_BUILD_TARGET / cargo config
# `build.target = "wasm32..."` would otherwise produce a wasm we cannot exec.
HOST_TRIPLE="$(rustc -vV | awk '/^host:/{print $2}')"
unset CARGO_BUILD_TARGET || true
cargo run -q -p zipper-core --target "$HOST_TRIPLE" --example extract_introchord -- ref/extracted/main/Globals.luac

echo "==> fixtures ready:"
ls -l crates/zipper-core/data/*.bin crates/zipper-core/data/*.json
