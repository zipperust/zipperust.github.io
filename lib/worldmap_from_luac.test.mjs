/**
 * Golden test: ref/Zipper.pdx/main.pdz → ZMAP === crates/zipper-core/data/worldmap.bin
 *
 * Local-only (needs ref/). Skip quietly if the PDX is missing.
 *
 *   node www/lib/worldmap_from_luac.test.mjs
 */

import { readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

// Repo root = nearest ancestor holding Cargo.toml. Robust to `lib/` living at
// the repo root (public tree) or under `www/lib/` (private demo tree).
function findRoot(start) {
  for (let d = start; ; d = dirname(d)) {
    if (existsSync(join(d, "Cargo.toml"))) return d;
    if (dirname(d) === d) throw new Error("repo root (Cargo.toml) not found");
  }
}
const root = findRoot(__dirname);

const pdzPath = join(root, "ref/Zipper.pdx/main.pdz");
const goldenPath = join(root, "crates/zipper-core/data/worldmap.bin");

if (!existsSync(pdzPath)) {
  console.log("skip: missing ref/Zipper.pdx/main.pdz");
  process.exit(0);
}
if (!existsSync(goldenPath)) {
  console.error("missing golden", goldenPath);
  process.exit(1);
}

const { extractPdzAsync, getWorldmapLuac } = await import("./pdz.js");
const { worldmapBinFromLuac } = await import("./worldmap_from_luac.js");

const pdz = new Uint8Array(readFileSync(pdzPath));
const entries = await extractPdzAsync(pdz);
console.log("PDZ entries:", entries.length);
const luac = getWorldmapLuac(entries);
console.log("worldmap.luac bytes:", luac.length);

const t0 = Date.now();
const produced = worldmapBinFromLuac(luac);
console.log("encode ms:", Date.now() - t0);

const golden = new Uint8Array(readFileSync(goldenPath));
console.log("produced:", produced.length, "golden:", golden.length);
if (produced.length !== golden.length) {
  console.error("length mismatch");
  process.exit(1);
}
for (let i = 0; i < produced.length; i++) {
  if (produced[i] !== golden[i]) {
    console.error(
      `byte mismatch at ${i}: got 0x${produced[i].toString(16)} want 0x${golden[i].toString(16)}`,
    );
    process.exit(1);
  }
}
console.log("OK: ZMAP matches golden worldmap.bin");
