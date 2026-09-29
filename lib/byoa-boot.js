/**
 * BYOA: load worldmap + dialogs + assets + music from a user Zipper.pdx path
 * map, or from the IndexedDB cache. Installs everything into the wasm app.
 */

import {
  extractPdzAsync,
  getWorldmapLuac,
  getScriptLuac,
  getGlobalsLuac,
  getMainLuac,
} from "./pdz.js";
import { worldmapBinFromLuac } from "./worldmap_from_luac.js";
import { dialogsFromScriptLuac } from "./dialogs_from_luac.js";
import {
  ASSET_MANIFEST,
  INTRO_LUAC_PATHS,
  SHO_MIDI_PATHS,
} from "./asset-manifest.js";
import {
  findBytes,
  readPdxVersion,
  validatePdxTree,
} from "./pdx-ingest.js";
import {
  BYOA_CACHE_SCHEMA,
  clearByoaBundle,
  loadByoaBundle,
  saveByoaBundle,
} from "./asset-cache.js";

/**
 * @param {*} app  ZipperApp
 * @param {Uint8Array} worldmap
 */
export function installWorldmap(app, worldmap) {
  app.loadWorldmap(worldmap);
  if (!app.worldmapLoaded()) {
    throw new Error("worldmap failed to install");
  }
}

/**
 * @param {*} app
 * @param {string[][]} dialogs
 */
export function installDialogs(app, dialogs) {
  app.loadDialogs(dialogs);
  if (!app.dialogsLoaded()) {
    throw new Error("dialogs failed to install");
  }
}

/**
 * Intro music: pass the raw `Globals.luac` bytes; zipper-core extracts the notes.
 * @param {*} app
 * @param {Uint8Array} luac
 */
export function installIntroMusic(app, luac) {
  if (typeof app.loadIntroMusicFromLuac !== "function") return;
  app.loadIntroMusicFromLuac(luac);
  if (typeof app.introMusicLoaded === "function" && !app.introMusicLoaded()) {
    throw new Error("intro music failed to install");
  }
}

/**
 * Ending-credit prose: pass the raw `main.luac` bytes; zipper-core extracts the
 * authored cards. Soft: the ending still runs (credits just stay blank).
 * @param {*} app
 * @param {Uint8Array} luac
 */
export function installCredits(app, luac) {
  if (!luac || typeof app.loadCreditsFromLuac !== "function") return;
  try {
    app.loadCreditsFromLuac(luac);
    if (typeof app.creditsLoaded === "function" && !app.creditsLoaded()) {
      console.warn("zipper-rust: main.luac present but no ending credits found");
    }
  } catch (err) {
    console.warn("zipper-rust: ending credits load failed", err);
  }
}

/**
 * Optional victory `Sho.mid`. Soft: silently ignored when absent/undecodable.
 * @param {*} app
 * @param {Uint8Array|null} bytes
 */
export function installMidi(app, bytes) {
  if (!bytes || typeof app.loadMidi !== "function") return;
  try {
    app.loadMidi("sho", bytes);
    if (typeof app.shoMidiLoaded === "function" && !app.shoMidiLoaded()) {
      console.warn("zipper-rust: Sho.mid present but not accepted by core");
    }
  } catch (err) {
    console.warn("zipper-rust: Sho.mid load failed", err);
  }
}

/** @param {string} kind @param {string} name */
export function assetKey(kind, name) {
  return `${kind}:${name}`;
}

/** @param {string} key */
function splitAssetKey(key) {
  const i = key.indexOf(":");
  return { kind: key.slice(0, i), name: key.slice(i + 1) };
}

/**
 * @param {*} app
 * @param {string} name
 * @param {'pdi'|'pdt'|'pda'|'pft'} kind
 * @param {Uint8Array} data
 */
export function installAsset(app, name, kind, data) {
  switch (kind) {
    case "pdi":
      app.loadPdi(name, data);
      break;
    case "pdt":
      app.loadPdt(name, data);
      break;
    case "pda":
      app.loadPda(name, data);
      break;
    case "pft":
      app.loadPft(name, data);
      break;
    default:
      throw new Error(`unknown asset kind: ${kind}`);
  }
}

/**
 * @param {*} app
 * @param {import('./asset-cache.js').ByoaBundle} bundle
 * @param {(msg: string) => void} [onProgress]
 */
export function applyBundle(app, bundle, onProgress) {
  onProgress?.("Loading cached worldmap…");
  installWorldmap(app, bundle.worldmap);
  onProgress?.("Loading cached dialogs…");
  if (!bundle.dialogs || !Array.isArray(bundle.dialogs) || !bundle.dialogs.length) {
    throw new Error("cached BYOA bundle missing dialogs — clear saved assets and re-drop Zipper.pdx");
  }
  installDialogs(app, bundle.dialogs);
  if (bundle.introLuac) {
    onProgress?.("Loading cached intro music…");
    try {
      installIntroMusic(app, bundle.introLuac);
    } catch (err) {
      console.warn("zipper-rust: cached intro music failed", err);
    }
  }
  if (bundle.creditsLuac) installCredits(app, bundle.creditsLuac);
  if (bundle.midi) installMidi(app, bundle.midi);
  const entries = Object.entries(bundle.assets);
  let i = 0;
  for (const [key, { kind, data }] of entries) {
    i++;
    onProgress?.(`Loading cached assets… ${i}/${entries.length}`, {
      done: i,
      total: entries.length,
    });
    const name = splitAssetKey(key).name || key;
    try {
      installAsset(app, name, kind, data);
    } catch (err) {
      console.warn(`zipper-rust: cached asset failed: ${key}`, err);
    }
  }
}

/**
 * @param {Map<string, Uint8Array>} pathMap
 * @param {*} app
 * @param {string} portVersion
 * @param {(msg: string) => void} [onProgress]
 * @returns {Promise<import('./asset-cache.js').ByoaBundle>}
 */
export async function ingestPathMap(pathMap, app, portVersion, onProgress) {
  const valid = validatePdxTree(pathMap);
  if (!valid.ok) throw new Error(valid.reason);

  onProgress?.("Extracting worldmap + dialogs from main.pdz…");
  const mainPdz = pathMap.get("main.pdz");
  if (!mainPdz) throw new Error("missing main.pdz");

  let entries;
  try {
    entries = await extractPdzAsync(mainPdz);
  } catch (err) {
    const msg = err && err.message ? err.message : String(err);
    if (/encrypted|DRM/i.test(msg)) {
      throw new Error(
        "This Zipper.pdx has an encrypted main.pdz (Catalog DRM). Zipper-Rust needs an unencrypted sideload/Data Disk copy (e.g. build 1.10).",
      );
    }
    throw err;
  }

  const worldmap = worldmapBinFromLuac(getWorldmapLuac(entries));
  installWorldmap(app, worldmap);
  onProgress?.("Worldmap ready.");

  const dialogs = dialogsFromScriptLuac(getScriptLuac(entries));
  installDialogs(app, dialogs);
  onProgress?.(`Dialogs ready (${dialogs.length} scripts).`);

  // Intro music (Globals.luac). Required in-stock; soft-fail on odd dumps.
  let introLuac = null;
  try {
    introLuac = getGlobalsLuac(entries);
    installIntroMusic(app, introLuac);
    onProgress?.("Intro music ready.");
  } catch (err) {
    console.warn("zipper-rust: intro music unavailable", err);
  }

  // Ending-credit prose (main.luac). Soft: blank credits on odd dumps.
  let creditsLuac = null;
  try {
    creditsLuac = getMainLuac(entries);
    installCredits(app, creditsLuac);
    onProgress?.("Ending credits ready.");
  } catch (err) {
    console.warn("zipper-rust: ending credits unavailable", err);
  }

  // Victory Sho.mid — optional.
  const midi = findBytes(pathMap, SHO_MIDI_PATHS);
  installMidi(app, midi);

  /** @type {Record<string, { kind: string, data: Uint8Array }>} */
  const assets = {};
  const missingRequired = [];
  let done = 0;
  const total = ASSET_MANIFEST.length;

  for (const entry of ASSET_MANIFEST) {
    done++;
    onProgress?.(`Loading ${entry.name}… (${done}/${total})`, { done, total });
    const bytes = findBytes(pathMap, entry.paths);
    if (!bytes) {
      if (entry.required) missingRequired.push(`${entry.name} (${entry.paths[0]})`);
      else console.warn(`zipper-rust: optional asset missing: ${entry.name}`);
      continue;
    }
    try {
      installAsset(app, entry.name, entry.kind, bytes);
      assets[assetKey(entry.kind, entry.name)] = { kind: entry.kind, data: bytes };
    } catch (err) {
      if (entry.required) {
        throw new Error(`Failed to load ${entry.name}: ${err.message || err}`);
      }
      console.warn(`zipper-rust: asset decode failed: ${entry.name}`, err);
    }
  }

  if (missingRequired.length) {
    throw new Error(
      `Missing required files in Zipper.pdx:\n• ${missingRequired.join("\n• ")}`,
    );
  }

  const bundle = {
    schema: BYOA_CACHE_SCHEMA,
    portVersion: portVersion || "",
    pdxVersion: readPdxVersion(pathMap),
    savedAt: Date.now(),
    worldmap,
    dialogs,
    introLuac,
    creditsLuac,
    midi: midi || null,
    assets,
  };

  onProgress?.("Saving to browser storage…");
  try {
    await saveByoaBundle(bundle);
  } catch (err) {
    console.warn("zipper-rust: could not persist BYOA cache", err);
  }

  return bundle;
}

/**
 * @param {*} app
 * @param {string} portVersion
 * @param {(msg: string) => void} [onProgress]
 * @returns {Promise<boolean>} true if restored
 */
export async function tryRestoreFromCache(app, portVersion, onProgress) {
  const bundle = await loadByoaBundle(portVersion);
  if (!bundle) return false;
  onProgress?.("Restoring your Zipper.pdx from browser storage…");
  applyBundle(app, bundle, onProgress);
  console.info(
    `zipper-rust: restored BYOA cache (pdxversion=${bundle.pdxVersion}, assets=${Object.keys(bundle.assets).length}, dialogs=${bundle.dialogs?.length || 0})`,
  );
  return true;
}

export { clearByoaBundle };
