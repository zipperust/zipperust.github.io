/**
 * Golden: script.luac → 31 dialog scripts / 258 lines.
 *
 * Prefers staged www/assets/demo/script.luac; falls back to ref/ or PDZ.
 *
 *   node www/lib/dialogs_from_luac.test.mjs
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

const staged = join(root, "www/assets/demo/script.luac");
const extracted = join(root, "ref/extracted/main/script.luac");
const pdzPath = join(root, "ref/Zipper.pdx/main.pdz");
const goldenJson = join(root, "crates/zipper-core/data/dialogs.json");

const { dialogsFromScriptLuac } = await import("./dialogs_from_luac.js");

async function loadScriptLuac() {
  if (existsSync(staged)) return new Uint8Array(readFileSync(staged));
  if (existsSync(extracted)) return new Uint8Array(readFileSync(extracted));
  if (!existsSync(pdzPath)) {
    console.log("skip: no script.luac / main.pdz");
    process.exit(0);
  }
  const { extractPdzAsync, getScriptLuac } = await import("./pdz.js");
  const entries = await extractPdzAsync(new Uint8Array(readFileSync(pdzPath)));
  return getScriptLuac(entries);
}

const luac = await loadScriptLuac();
const scripts = dialogsFromScriptLuac(luac);
const total = scripts.reduce((n, s) => n + s.length, 0);
console.log("scripts:", scripts.length, "lines:", total);
console.log("s1 first:", scripts[0]?.[0]);
console.log("s10 first:", scripts[9]?.[0]);

if (scripts.length !== 31) {
  console.error("expected 31 scripts");
  process.exit(1);
}
if (total !== 258) {
  console.error("expected 258 lines");
  process.exit(1);
}
if (!scripts[0][0].includes("Henchmen")) {
  console.error("script 1 first line mismatch");
  process.exit(1);
}
if (!scripts[9][0].includes("Impossible")) {
  console.error("script 10 first line mismatch");
  process.exit(1);
}

if (existsSync(goldenJson)) {
  const golden = JSON.parse(readFileSync(goldenJson, "utf8"));
  if (JSON.stringify(scripts) !== JSON.stringify(golden)) {
    console.error("mismatch vs crates/zipper-core/data/dialogs.json");
    process.exit(1);
  }
  console.log("OK: matches dialogs.json");
} else {
  console.log("OK: shape checks passed (no dialogs.json)");
}
