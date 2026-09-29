/**
 * Derive the local golden fixtures from your own Zipper.pdx (BYOA).
 *
 * Content-free tooling: it only transforms the copy you supply under ref/.
 * Never committed — the outputs are gitignored (see .gitignore).
 *
 *   node scripts/gen-fixtures.mjs
 *
 * Writes:
 *   crates/zipper-core/data/worldmap.bin
 *   crates/zipper-core/data/dialogs.json
 *   ref/extracted/main/*.luac   (script, Globals, main, worldmap)
 */

import { readFileSync, writeFileSync, mkdirSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, "..");

const {
  extractPdzAsync,
  getWorldmapLuac,
  getScriptLuac,
  getGlobalsLuac,
  getMainLuac,
} = await import("../lib/pdz.js");
const { worldmapBinFromLuac } = await import("../lib/worldmap_from_luac.js");
const { dialogsFromScriptLuac } = await import("../lib/dialogs_from_luac.js");

const pdzPath = join(root, "ref/Zipper.pdx/main.pdz");
if (!existsSync(pdzPath)) {
  console.error(`missing ${pdzPath} — run scripts/gen-fixtures.sh`);
  process.exit(1);
}

const entries = await extractPdzAsync(new Uint8Array(readFileSync(pdzPath)));

const dataDir = join(root, "crates/zipper-core/data");
const extractDir = join(root, "ref/extracted/main");
mkdirSync(dataDir, { recursive: true });
mkdirSync(extractDir, { recursive: true });

/** @returns {Uint8Array|null} */
function dumpLuac(getter, name) {
  try {
    const bytes = getter(entries);
    writeFileSync(join(extractDir, `${name}.luac`), bytes);
    return bytes;
  } catch {
    console.warn(`note: no "${name}" luac entry in main.pdz`);
    return null;
  }
}

const worldmap = dumpLuac(getWorldmapLuac, "worldmap");
if (worldmap) {
  writeFileSync(join(dataDir, "worldmap.bin"), worldmapBinFromLuac(worldmap));
  console.log("wrote crates/zipper-core/data/worldmap.bin");
}

const script = dumpLuac(getScriptLuac, "script");
if (script) {
  const dialogs = dialogsFromScriptLuac(script);
  writeFileSync(join(dataDir, "dialogs.json"), JSON.stringify(dialogs));
  console.log(`wrote crates/zipper-core/data/dialogs.json (${dialogs.length} scripts)`);
}

dumpLuac(getGlobalsLuac, "Globals");
dumpLuac(getMainLuac, "main");
