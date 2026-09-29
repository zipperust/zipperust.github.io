// Query stamped by scripts/build.sh (PORT_VERSION) so browsers don't keep a stale pkg.
import init, { ZipperApp } from "./pkg/zipper_wasm.js?v=0.39o";

import { tryRestoreFromCache, ingestPathMap, clearByoaBundle } from "./lib/byoa-boot.js";
import {
  pathMapFromZip,
  pathMapFromFileList,
  pathMapFromDirectoryEntry,
  pathMapFromDirectoryHandle,
} from "./lib/pdx-ingest.js";

/** True once the BYOA gate has been satisfied (cache or fresh ingest). */
let byoaReady = false;

/** Actions the game understands (`setButton` names). Menu is Esc (host-only, not remappable). */
const ACTIONS = [
  { id: "up", label: "Up" },
  { id: "down", label: "Down" },
  { id: "left", label: "Left" },
  { id: "right", label: "Right" },
  { id: "a", label: "A (zip)" },
  { id: "b", label: "B (reset)" },
];

/**
 * Default action → physical key (`KeyboardEvent.code`).
 * Move = IJKL (same glyphs on QWERTY and AZERTY). A/B = X + Z (AZERTY: X + W).
 * Extra aliases (arrows, Space) stay always-on and are not remapped.
 * Host menu = Esc (fixed; not a game button).
 */
const DEFAULT_BINDS = {
  up: "KeyI",
  down: "KeyK",
  left: "KeyJ",
  right: "KeyL",
  a: "KeyX",
  b: "KeyZ",
};

/** Fixed extras keyed by `KeyboardEvent.code` (layout-independent). Esc opens the host menu. */
const FIXED_ALIASES = {
  ArrowUp: "up",
  ArrowDown: "down",
  ArrowLeft: "left",
  ArrowRight: "right",
  Space: "a",
};

/**
 * Remappable binds in localStorage (per origin).
 * v4 stores `KeyboardEvent.code` (move=IJKL, A=KeyX, B=KeyZ). Older saves ignored.
 */
const STORAGE_KEY = "zipper.keybinds.v4";

/**
 * Mid-run resume blob (`utils.lua` datastore `"save"`).
 * See docs/mid-run-save.md. Dialog counters use a separate key later.
 */
const SAVE_KEY = "zipper.save.v1";

/**
 * Local high-score board (Catalog stand-in). Survives wins / Delete save / reload.
 * See docs/highscores.md.
 */
const HS_KEY = "zipper.hs.v1";

/** Read raw mid-run save JSON from localStorage, or null. */
function readMidRunSaveRaw() {
  try {
    return localStorage.getItem(SAVE_KEY);
  } catch (_) {
    return null;
  }
}

/** Read raw high-score JSON from localStorage, or null. */
function readHighscoresRaw() {
  try {
    return localStorage.getItem(HS_KEY);
  } catch (_) {
    return null;
  }
}

/** Persist high-score JSON. */
function persistHighscores(json) {
  if (json == null || typeof json !== "string") return;
  try {
    localStorage.setItem(HS_KEY, json);
  } catch (err) {
    console.warn("zipper highscores persist failed", err);
  }
}

/** Drain core dirty high scores after a win submit. */
function flushHsIfDirty(app) {
  if (!app || typeof app.takeHsFlush !== "function") return;
  try {
    if (typeof app.hsNeedsFlush === "function" && !app.hsNeedsFlush()) {
      return;
    }
    const json = app.takeHsFlush();
    if (json != null) persistHighscores(json);
  } catch (err) {
    console.warn("zipper highscores flush failed", err);
  }
}

/**
 * Light parse for boot seed / loadsave gate. Full restore is `app.applySave`.
 * @returns {{ loadsave: number, seed: number, version: number } | null}
 */
function parseMidRunSave(raw) {
  if (!raw || typeof raw !== "string") return null;
  try {
    const o = JSON.parse(raw);
    if (!o || typeof o !== "object") return null;
    const loadsave = Number(o.loadsave) || 0;
    const seed = Number(o.seed);
    const version = Number(o.version) || 0;
    if (!Number.isFinite(seed)) return null;
    if (version < 1) return null;
    return { loadsave, seed: seed >>> 0, version };
  } catch (_) {
    return null;
  }
}

/** Persist mid-run save JSON (or clear when loadsave cleared by core). */
function persistMidRunSave(json) {
  if (json == null || typeof json !== "string") return;
  try {
    localStorage.setItem(SAVE_KEY, json);
  } catch (err) {
    console.warn("zipper save persist failed", err);
  }
}

/** Drain core dirty save after exit / delete / restore. */
function flushSaveIfDirty(app) {
  if (!app || typeof app.takeSaveFlush !== "function") return;
  try {
    if (typeof app.saveNeedsFlush === "function" && !app.saveNeedsFlush()) {
      return;
    }
    const json = app.takeSaveFlush();
    if (json != null) persistMidRunSave(json);
  } catch (err) {
    console.warn("zipper save flush failed", err);
  }
}

/** `gameWillTerminate` stand-in — visibility hidden / pagehide / menu Reboot. */
function flushTerminateSave(app) {
  if (!app || typeof app.takeTerminateSave !== "function") return;
  try {
    const json = app.takeTerminateSave();
    if (json != null) persistMidRunSave(json);
  } catch (err) {
    console.warn("zipper terminate save failed", err);
  }
}

/** QWERTY printed labels for common codes (fallback when layout map unavailable). */
const CODE_LABEL_QWERTY = {
  KeyI: "I",
  KeyJ: "J",
  KeyK: "K",
  KeyL: "L",
  KeyW: "W",
  KeyA: "A",
  KeyS: "S",
  KeyD: "D",
  KeyZ: "Z",
  KeyX: "X",
  KeyM: "M",
  KeyQ: "Q",
  KeyF: "F",
  Space: "Space",
  Escape: "Esc",
};

/** French AZERTY printed labels for the same physical codes. */
const CODE_LABEL_AZERTY = {
  KeyI: "I",
  KeyJ: "J",
  KeyK: "K",
  KeyL: "L",
  KeyW: "Z",
  KeyA: "Q",
  KeyS: "S",
  KeyD: "D",
  KeyZ: "W",
  KeyX: "X",
  KeyM: ",",
  KeyQ: "A",
  KeyF: "F",
  Space: "Space",
  Escape: "Esc",
};

/** Live `code → printed glyph` from Keyboard API or observed keydowns. */
let layoutLabels = Object.create(null);

function languageLooksAzerty() {
  const langs = [navigator.language, ...(navigator.languages || [])]
    .filter(Boolean)
    .map((l) => String(l).toLowerCase());
  // Prefer European French / Belgian — `fr-CA` is usually QWERTY Canadian French.
  return langs.some(
    (l) =>
      l === "fr" ||
      l === "fr-fr" ||
      l.startsWith("fr-fr") ||
      l === "fr-be" ||
      l.startsWith("fr-be") ||
      l === "be" ||
      l.startsWith("be-") ||
      (l.startsWith("fr-") && !l.startsWith("fr-ca") && !l.startsWith("fr-us")),
  );
}

function fallbackCodeLabel(code) {
  const table = languageLooksAzerty() ? CODE_LABEL_AZERTY : CODE_LABEL_QWERTY;
  if (table[code]) return table[code];
  if (code.startsWith("Key") && code.length === 4) return code.slice(3);
  if (code.startsWith("Digit") && code.length === 6) return code.slice(5);
  return code;
}

function sanitizeBinds(raw) {
  const out = { ...DEFAULT_BINDS };
  if (!raw || typeof raw !== "object") return out;
  for (const { id } of ACTIONS) {
    if (typeof raw[id] === "string" && raw[id].length > 0) {
      out[id] = normalizeCode(raw[id]);
    }
  }
  return out;
}

function bindsEqual(a, b) {
  for (const { id } of ACTIONS) {
    if (a[id] !== b[id]) return false;
  }
  return true;
}

function loadBinds() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULT_BINDS };
    const parsed = JSON.parse(raw);
    const out = sanitizeBinds(parsed);
    // Re-write normalized JSON so older / messy saves stay readable.
    if (!bindsEqual(out, parsed)) {
      saveBinds(out);
    }
    return out;
  } catch (err) {
    console.warn("could not load keybinds; using defaults", err);
    return { ...DEFAULT_BINDS };
  }
}

function saveBinds(binds) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(sanitizeBinds(binds)));
  } catch (err) {
    // Private mode / quota / blocked storage — binds stay in-memory only.
    console.warn("could not save keybinds", err);
  }
}

/** Normalize a stored bind to a `KeyboardEvent.code`-style id. */
function normalizeCode(code) {
  if (typeof code !== "string" || !code) return code;
  // Legacy v1 single-letter binds → QWERTY physical codes (best-effort).
  if (code.length === 1) {
    const ch = code.toLowerCase();
    if (ch >= "a" && ch <= "z") return `Key${ch.toUpperCase()}`;
    if (ch >= "0" && ch <= "9") return `Digit${ch}`;
    if (ch === " ") return "Space";
  }
  return code;
}

/** Printed label for a physical code on the current layout. */
function displayKey(code) {
  if (!code) return "?";
  const live = layoutLabels[code];
  if (live) return live.length === 1 ? live.toUpperCase() : live;
  return fallbackCodeLabel(code);
}

/** Remember what glyph a physical key produced (FR W on KeyZ, etc.). */
function noteLayoutFromEvent(e) {
  if (!e || !e.code) return false;
  const glyph = e.key;
  if (!glyph || glyph.length !== 1) return false;
  if (e.ctrlKey || e.metaKey || e.altKey) return false;
  const next = glyph.toLowerCase();
  if (layoutLabels[e.code] === next) return false;
  layoutLabels[e.code] = next;
  return true;
}

/** Build `KeyboardEvent.code` → action map from remappable binds + fixed aliases. */
function buildKeyMap(binds) {
  const map = { ...FIXED_ALIASES };
  for (const [action, code] of Object.entries(binds)) {
    map[normalizeCode(code)] = action;
  }
  return map;
}

/** Resolve a key event to a game action (physical code first). */
function actionFromEvent(keyMap, e) {
  if (e.code && keyMap[e.code]) return keyMap[e.code];
  // Rare fallback for older events without code.
  if (e.key === " ") return keyMap.Space;
  if (e.key === "Escape") return keyMap.Escape;
  return undefined;
}

/**
 * Prefer Keyboard Layout API when available (Chrome / Edge).
 * Fills `layoutLabels` so the Keys list shows layout glyphs (IJKL + A/B pair).
 */
async function refreshLayoutLabels() {
  try {
    if (!navigator.keyboard || typeof navigator.keyboard.getLayoutMap !== "function") {
      return false;
    }
    const map = await navigator.keyboard.getLayoutMap();
    const next = Object.create(null);
    for (const code of Object.keys(DEFAULT_BINDS).map((k) => DEFAULT_BINDS[k])) {
      const glyph = map.get(code);
      if (glyph) next[code] = glyph;
    }
    // Also label fixed / common codes used in the UI.
    for (const code of ["KeyF", "Space", "Escape", "KeyQ"]) {
      const glyph = map.get(code);
      if (glyph) next[code] = glyph;
    }
    layoutLabels = next;
    return true;
  } catch (err) {
    console.warn("keyboard layout map unavailable", err);
    return false;
  }
}

function setStatus(text, isError = false) {
  const el = document.getElementById("status");
  if (!el) return;
  el.textContent = text == null ? "" : String(text);
  el.classList.toggle("error", !!isError && !!text);
}

/**
 * Split the URL fragment into lowercase tokens (`&`, `,`, `/` separators).
 * Used for public `#seed=N` parsing (tools tokens are handled in wasm).
 * @returns {string[]}
 */
function urlFragmentTokens() {
  const raw = (window.location.hash || "").replace(/^#/, "").trim().toLowerCase();
  if (!raw) return [];
  return raw
    .split(/[&,]+/)
    .flatMap((p) => p.split("/"))
    .map((p) => p.trim())
    .filter(Boolean);
}

/**
 * Force run RNG seed from `#seed=N` in the fragment (u32).
 * First matching `seed=<digits>` wins; invalid / absent → null (epoch default).
 * Query `?seed=` is ignored. See docs/cheats.md / docs/url-fragment-seed.md.
 * @returns {number|null}
 */
function urlSeedFromHash() {
  for (const t of urlFragmentTokens()) {
    const m = /^seed=(\d+)$/.exec(t);
    if (!m) continue;
    return Number.parseInt(m[1], 10) >>> 0;
  }
  return null;
}

const MOBILE_LAYOUT_KEY = "zipper.mobileLayout.v1";
/** Master SFX volume 0..100; default 60 when unset. */
const SFX_VOLUME_KEY = "zipper.sfxVolume.v1";
const DEFAULT_SFX_VOLUME_PCT = 60;
/** Optional tools Invulnerability switch; default ON when unset. */
const DT_INVULN_KEY = "zipper.dtInvuln.v1";
/** Optional tools Castle key switch; default ON when unset. */
const DT_KEY_KEY = "zipper.dtKey.v1";
/** Snap CSS canvas size to whole-number multiples of 400×240; default ON. */
const INTEGER_SCALE_KEY = "zipper.integerScale.v1";
/**
 * Desktop preferred device-pixel scale N (each FB pixel → N device pixels).
 * Default ≈ 3× CSS width on the current DPR (`round(3 * dpr)`).
 * v2 replaces CSS-multiplier `zipper.desktopZoom.v1`.
 */
const DESKTOP_DEVICE_SCALE_KEY = "zipper.desktopDeviceScale.v1";
const LEGACY_DESKTOP_ZOOM_KEY = "zipper.desktopZoom.v1";
const DEFAULT_DESKTOP_CSS_ZOOM = 3;
const MIN_DESKTOP_DEVICE_SCALE = 1;
const MAX_DESKTOP_DEVICE_SCALE = 16;
/** Idle fade for desktop zoom +/- and stage crosshair (same timeout). */
const STAGE_ZOOM_IDLE_MS = 4000;

function readIntegerScalePref() {
  try {
    const v = localStorage.getItem(INTEGER_SCALE_KEY);
    if (v === "0") return false;
    if (v === "1") return true;
  } catch {
    /* ignore */
  }
  return true;
}

function writeIntegerScalePref(on) {
  try {
    localStorage.setItem(INTEGER_SCALE_KEY, on ? "1" : "0");
  } catch {
    /* ignore */
  }
}

function defaultDesktopDeviceScale() {
  const dpr = window.devicePixelRatio || 1;
  return Math.max(
    MIN_DESKTOP_DEVICE_SCALE,
    Math.min(MAX_DESKTOP_DEVICE_SCALE, Math.round(DEFAULT_DESKTOP_CSS_ZOOM * dpr)),
  );
}

function clampDesktopDeviceScale(n) {
  return Math.max(MIN_DESKTOP_DEVICE_SCALE, Math.min(MAX_DESKTOP_DEVICE_SCALE, n));
}

function readDesktopDeviceScalePref() {
  try {
    const v = localStorage.getItem(DESKTOP_DEVICE_SCALE_KEY);
    if (v != null) {
      const n = Number.parseInt(v, 10);
      if (Number.isFinite(n)) return clampDesktopDeviceScale(n);
    }
    // Migrate old CSS-multiplier pref (1..8) → device scale once.
    const legacy = localStorage.getItem(LEGACY_DESKTOP_ZOOM_KEY);
    if (legacy != null) {
      const cssZoom = Number.parseInt(legacy, 10);
      if (Number.isFinite(cssZoom)) {
        const dpr = window.devicePixelRatio || 1;
        const n = clampDesktopDeviceScale(Math.round(cssZoom * dpr));
        writeDesktopDeviceScalePref(n);
        return n;
      }
    }
  } catch {
    /* ignore */
  }
  return defaultDesktopDeviceScale();
}

function writeDesktopDeviceScalePref(n) {
  try {
    localStorage.setItem(DESKTOP_DEVICE_SCALE_KEY, String(n));
  } catch {
    /* ignore */
  }
}

let integerScaleOnly = readIntegerScalePref();
/** Preferred desktop device-pixel scale (may be clamped down to fit). */
let desktopDeviceScale = readDesktopDeviceScalePref();
let stageZoomHideTimer = 0;
let stageCursorHideTimer = 0;
/** Last applied / max-fit desktop device-pixel scales (for +/- enable state). */
let lastDesktopDeviceScale = 0;
let lastDesktopMaxDeviceScale = 0;

function readMobileLayoutPref() {
  try {
    const v = localStorage.getItem(MOBILE_LAYOUT_KEY);
    if (v === "1") return true;
    if (v === "0") return false;
  } catch {
    /* ignore */
  }
  return null;
}

function writeMobileLayoutPref(on) {
  try {
    localStorage.setItem(MOBILE_LAYOUT_KEY, on ? "1" : "0");
  } catch {
    /* ignore */
  }
}

function readSfxVolumePct() {
  try {
    const v = localStorage.getItem(SFX_VOLUME_KEY);
    if (v == null) return DEFAULT_SFX_VOLUME_PCT;
    const n = Number.parseInt(v, 10);
    if (Number.isFinite(n)) return Math.max(0, Math.min(100, n));
  } catch {
    /* ignore */
  }
  return DEFAULT_SFX_VOLUME_PCT;
}

function writeSfxVolumePct(pct) {
  try {
    localStorage.setItem(SFX_VOLUME_KEY, String(pct));
  } catch {
    /* ignore */
  }
}

/** Read a boolean localStorage pref; `defaultOn` when missing / unreadable. */
function readBoolPref(key, defaultOn) {
  try {
    const v = localStorage.getItem(key);
    if (v === "0") return false;
    if (v === "1") return true;
  } catch {
    /* ignore */
  }
  return defaultOn;
}

function writeBoolPref(key, on) {
  try {
    localStorage.setItem(key, on ? "1" : "0");
  } catch {
    /* ignore */
  }
}

function readDtInvulnPref() {
  return readBoolPref(DT_INVULN_KEY, true);
}

function writeDtInvulnPref(on) {
  writeBoolPref(DT_INVULN_KEY, on);
}

function readDtKeyPref() {
  return readBoolPref(DT_KEY_KEY, true);
}

function writeDtKeyPref(on) {
  writeBoolPref(DT_KEY_KEY, on);
}

/** Device heuristic only (no URL / saved preference). */
function devicePrefersMobilePlay() {
  if (typeof window.matchMedia !== "function") return false;
  const coarse = window.matchMedia("(pointer: coarse)").matches;
  const noHover = window.matchMedia("(hover: none)").matches;
  const narrow = window.matchMedia("(max-width: 900px)").matches;
  return (coarse || noHover) && narrow;
}

/**
 * Whether to use the fullscreen touch overlay.
 * Priority: `?mobile=1|0` → saved burger-menu switch → device heuristic.
 */
function prefersMobilePlay() {
  const params = new URLSearchParams(window.location.search);
  const forced = params.get("mobile");
  if (forced === "1" || forced === "true") return true;
  if (forced === "0" || forced === "false") return false;
  const pref = readMobileLayoutPref();
  if (pref !== null) return pref;
  return devicePrefersMobilePlay();
}

function isMobilePlay() {
  return document.body.classList.contains("mobile-play");
}

function isImmersive() {
  return document.body.classList.contains("immersive") || isMobilePlay();
}

/**
 * Request browser fullscreen when available. Failures are non-fatal
 * (iOS Safari often blocks programmatic fullscreen).
 */
async function requestBrowserFullscreen() {
  try {
    if (!document.fullscreenElement && document.documentElement.requestFullscreen) {
      await document.documentElement.requestFullscreen();
    }
  } catch (err) {
    console.warn("browser fullscreen unavailable", err);
  }
}

async function exitBrowserFullscreen() {
  try {
    if (document.fullscreenElement) {
      await document.exitFullscreen();
    }
  } catch (err) {
    console.warn("exit fullscreen failed", err);
  }
}

function isStandaloneDisplay() {
  return (
    window.navigator.standalone === true ||
    (typeof window.matchMedia === "function" &&
      window.matchMedia("(display-mode: standalone)").matches)
  );
}

function isLikelyIosSafari() {
  const ua = navigator.userAgent || "";
  const iOS = /iPad|iPhone|iPod/.test(ua) || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
  const webkit = /WebKit/.test(ua);
  const notOther = !/CriOS|FxiOS|EdgiOS|OPiOS|DuckDuckGo/.test(ua);
  return iOS && webkit && notOther;
}

/**
 * Drive mobile-play layout from visualViewport so Safari's URL / tool bars
 * don't silently shrink the usable area (especially landscape).
 */
function syncMobileViewport() {
  if (!isMobilePlay()) return;
  const vv = window.visualViewport;
  const width = vv?.width ?? window.innerWidth;
  const height = vv?.height ?? window.innerHeight;
  const top = vv?.offsetTop ?? 0;
  const left = vv?.offsetLeft ?? 0;
  const root = document.documentElement;
  root.style.setProperty("--vv-width", `${width}px`);
  root.style.setProperty("--vv-height", `${height}px`);
  root.style.setProperty("--vv-offset-top", `${top}px`);
  root.style.setProperty("--vv-offset-left", `${left}px`);
  // Also mirror on body so existing body.mobile-play var fallbacks resolve.
  document.body.style.setProperty("--vv-width", `${width}px`);
  document.body.style.setProperty("--vv-height", `${height}px`);
  document.body.style.setProperty("--vv-offset-top", `${top}px`);
  document.body.style.setProperty("--vv-offset-left", `${left}px`);
}

function clearCanvasDisplaySize(canvas) {
  canvas.style.width = "";
  canvas.style.height = "";
  canvas.style.maxWidth = "";
  canvas.style.maxHeight = "";
}

/** Content-box width of an element (clientWidth minus horizontal padding). */
function contentBoxWidth(el) {
  if (!el) return 0;
  const cs = getComputedStyle(el);
  return Math.max(
    0,
    el.clientWidth -
      (Number.parseFloat(cs.paddingLeft) || 0) -
      (Number.parseFloat(cs.paddingRight) || 0),
  );
}

/**
 * Snap #screen CSS size so each framebuffer pixel covers an integer number of
 * *device* pixels (Retina-aware). CSS size is (N×400)/dpr × (N×240)/dpr.
 * Desktop chrome uses preferred `desktopDeviceScale`; immersive uses the
 * largest integer fit. **Mobile-play always free-contains** (CSS letterbox) —
 * integer N× on a phone often leaves the LCD tiny in the letterbox. When the
 * pref is off (or mobile-play), clear inline sizes.
 */
function syncCanvasDisplaySize() {
  const canvas = document.getElementById("screen");
  if (!canvas) return;

  // Mobile touch layout: ignore integer-scale pref; use CSS contain sizing.
  if (isMobilePlay() || !integerScaleOnly) {
    clearCanvasDisplaySize(canvas);
    lastDesktopDeviceScale = 0;
    lastDesktopMaxDeviceScale = 0;
    syncStageZoomUi();
    return;
  }

  const desktopChrome = !document.body.classList.contains("immersive");

  let fitW;
  let fitH;
  if (document.body.classList.contains("immersive")) {
    fitW = window.innerWidth;
    fitH = window.innerHeight;
  } else {
    // Prefer the window over the stage content box: stage width is circular
    // with the canvas size. Cap by main's content box when present.
    const main = document.querySelector("main");
    fitW = contentBoxWidth(main) || window.innerWidth - 32;
    fitH = window.innerHeight;
  }

  const dpr = window.devicePixelRatio || 1;
  let maxScale = Math.floor(Math.min((fitW * dpr) / 400, (fitH * dpr) / 240));
  if (maxScale < 1) {
    clearCanvasDisplaySize(canvas);
    lastDesktopDeviceScale = 0;
    lastDesktopMaxDeviceScale = 0;
    syncStageZoomUi();
    return;
  }

  let scale = maxScale;
  if (desktopChrome) {
    scale = Math.min(maxScale, desktopDeviceScale);
    lastDesktopMaxDeviceScale = maxScale;
  } else {
    lastDesktopDeviceScale = 0;
    lastDesktopMaxDeviceScale = 0;
  }

  let cssW = (scale * 400) / dpr;
  let cssH = (scale * 240) / dpr;
  // Float noise / subpixel: step down if we still overflow the fit box.
  while (scale > 1 && (cssW > fitW + 0.5 || cssH > fitH + 0.5)) {
    scale -= 1;
    cssW = (scale * 400) / dpr;
    cssH = (scale * 240) / dpr;
  }
  if (desktopChrome) lastDesktopDeviceScale = scale;

  canvas.style.width = `${cssW}px`;
  canvas.style.height = `${cssH}px`;
  canvas.style.maxWidth = "none";
  canvas.style.maxHeight = "none";
  syncStageZoomUi();
}

function syncStageZoomUi() {
  const bar = document.getElementById("stage-zoom");
  if (!bar) return;
  const desktopChrome =
    !isMobilePlay() && !document.body.classList.contains("immersive");
  const show = desktopChrome && integerScaleOnly;
  bar.hidden = !show;
  if (!show) return;

  const label = document.getElementById("stage-zoom-label");
  const outBtn = document.getElementById("stage-zoom-out");
  const inBtn = document.getElementById("stage-zoom-in");
  // Label = applied device-pixel scale (may be below preferred if window is tight).
  const shown =
    lastDesktopDeviceScale > 0 ? lastDesktopDeviceScale : desktopDeviceScale;
  if (label) label.textContent = `${shown}×`;
  if (outBtn) {
    outBtn.disabled = lastDesktopDeviceScale <= MIN_DESKTOP_DEVICE_SCALE;
  }
  if (inBtn) {
    inBtn.disabled =
      lastDesktopDeviceScale >= lastDesktopMaxDeviceScale ||
      desktopDeviceScale >= MAX_DESKTOP_DEVICE_SCALE;
  }
}

function bumpDesktopDeviceScale(delta) {
  // Step from what's on screen so + always grows when headroom exists, even
  // if the preferred value was clamped by a smaller window earlier.
  const base =
    lastDesktopDeviceScale > 0 ? lastDesktopDeviceScale : desktopDeviceScale;
  const next = clampDesktopDeviceScale(base + delta);
  if (next === desktopDeviceScale && next === lastDesktopDeviceScale) return;
  desktopDeviceScale = next;
  writeDesktopDeviceScalePref(desktopDeviceScale);
  syncCanvasDisplaySize();
  showStageZoomControls();
}

function showStageZoomControls() {
  const bar = document.getElementById("stage-zoom");
  if (!bar || bar.hidden) return;
  bar.classList.add("is-visible");
  window.clearTimeout(stageZoomHideTimer);
  stageZoomHideTimer = window.setTimeout(() => {
    bar.classList.remove("is-visible");
  }, STAGE_ZOOM_IDLE_MS);
}

/** Reveal stage crosshair; hide after the same idle as zoom +/-. Desktop only. */
function showStageCursor() {
  const stage = document.querySelector(".stage");
  if (!stage || isMobilePlay()) return;
  stage.classList.remove("cursor-idle");
  window.clearTimeout(stageCursorHideTimer);
  stageCursorHideTimer = window.setTimeout(() => {
    stage.classList.add("cursor-idle");
  }, STAGE_ZOOM_IDLE_MS);
}

function onStagePointerActivity() {
  showStageZoomControls();
  showStageCursor();
}

function initStageZoomControls() {
  const stage = document.querySelector(".stage");
  const bar = document.getElementById("stage-zoom");
  const outBtn = document.getElementById("stage-zoom-out");
  const inBtn = document.getElementById("stage-zoom-in");
  if (!stage) return;

  outBtn?.addEventListener("click", (e) => {
    e.preventDefault();
    e.stopPropagation();
    bumpDesktopDeviceScale(-1);
  });
  inBtn?.addEventListener("click", (e) => {
    e.preventDefault();
    e.stopPropagation();
    bumpDesktopDeviceScale(1);
  });

  // Keep zoom chrome visible while interacting with the buttons themselves.
  if (bar) {
    bar.addEventListener("pointerdown", (e) => e.stopPropagation());
    bar.addEventListener("mousemove", onStagePointerActivity);
  }

  stage.addEventListener("mousemove", onStagePointerActivity);
  stage.addEventListener("mouseenter", onStagePointerActivity);
  // Leaving the stage restores the normal page cursor; next enter restarts idle.
  stage.addEventListener("mouseleave", () => {
    window.clearTimeout(stageCursorHideTimer);
    stage.classList.remove("cursor-idle");
  });

  syncStageZoomUi();
  if (bar && !bar.hidden) showStageZoomControls();
  showStageCursor();
}

/** visualViewport vars + integer (or free) canvas CSS size. */
function syncViewportLayout() {
  syncMobileViewport();
  syncCanvasDisplaySize();
}

/**
 * Best-effort: nudge iOS Safari to collapse the URL bar by briefly allowing
 * a taller document then scrolling. Works inconsistently in landscape (Safari
 * often keeps the bar), but helps portrait and some landscape cases after a
 * user gesture. Standalone / Add-to-Home-Screen is the reliable fix.
 */
function tryCollapseSafariChrome() {
  if (!isMobilePlay() || isStandaloneDisplay()) return;
  const html = document.documentElement;
  const body = document.body;
  const prevHtml = html.style.cssText;
  const prevBody = body.style.cssText;
  // Temporarily unlock scrolling so Safari will accept a chrome-collapse scroll.
  html.style.height = `${Math.max(window.innerHeight + 120, window.screen.height)}px`;
  html.style.overflow = "auto";
  body.style.position = "static";
  body.style.height = `${Math.max(window.innerHeight + 120, window.screen.height)}px`;
  body.style.overflow = "auto";
  body.style.touchAction = "auto";
  window.scrollTo(0, 1);
  requestAnimationFrame(() => {
    window.scrollTo(0, 0);
    html.style.cssText = prevHtml;
    // Re-apply mobile-play fixed layout vars after restoring inline styles.
    body.style.cssText = prevBody;
    if (isMobilePlay()) {
      body.classList.add("mobile-play");
      syncViewportLayout();
    }
  });
}

const IOS_TIP_KEY = "zipper.iosChromeTip.v1";

/** Session flag so a failed localStorage write still keeps the tip dismissed. */
let iosTipDismissed = false;

/**
 * Tip copy for the five most-spoken languages (en fallback).
 * Keys are BCP-47 primary subtags matched against `navigator.languages`.
 */
const IOS_TIP_I18N = {
  en: {
    bodyHtml:
      "To hide the Safari bar: tap the <strong>aA</strong> icon (left of the URL) → <strong>Hide Toolbar</strong>. Or: Share → <strong>Add to Home Screen</strong> (permanent fullscreen).",
    ok: "OK",
  },
  zh: {
    bodyHtml:
      "要隐藏 Safari 工具栏：点按网址左侧的 <strong>aA</strong> 图标 → <strong>隐藏工具栏</strong>。或者：共享 → <strong>添加到主屏幕</strong>（持久全屏）。",
    ok: "好",
  },
  hi: {
    bodyHtml:
      "Safari बार छिपाने के लिए: URL के बाईं ओर <strong>aA</strong> आइकन टैप करें → <strong>टूलबार छिपाएँ</strong>। या: शेयर → <strong>होम स्क्रीन पर जोड़ें</strong> (स्थायी फ़ुलस्क्रीन)।",
    ok: "ठीक",
  },
  es: {
    bodyHtml:
      "Para ocultar la barra de Safari: toca el icono <strong>aA</strong> (a la izquierda de la URL) → <strong>Ocultar barra de herramientas</strong>. O: Compartir → <strong>Añadir a la pantalla de inicio</strong> (pantalla completa permanente).",
    ok: "OK",
  },
  fr: {
    bodyHtml:
      "Pour masquer la barre Safari&nbsp;: icône <strong>aA</strong> (à gauche de l’URL) → <strong>Masquer la barre d’outils</strong>. Ou&nbsp;: Partager → <strong>Sur l’écran d’accueil</strong> (plein écran permanent).",
    ok: "OK",
  },
};

function pickIosTipLocale() {
  const tags = [];
  try {
    if (Array.isArray(navigator.languages)) tags.push(...navigator.languages);
  } catch {
    /* ignore */
  }
  if (navigator.language) tags.push(navigator.language);
  for (const raw of tags) {
    if (!raw || typeof raw !== "string") continue;
    const tag = raw.toLowerCase();
    const primary = tag.split("-")[0];
    if (IOS_TIP_I18N[tag]) return tag;
    if (IOS_TIP_I18N[primary]) return primary;
  }
  return "en";
}

function applyIosChromeTipI18n() {
  const tip = document.getElementById("ios-chrome-tip");
  if (!tip || tip.dataset.i18nApplied === "1") return;
  const locale = pickIosTipLocale();
  const strings = IOS_TIP_I18N[locale] || IOS_TIP_I18N.en;
  const body = document.getElementById("ios-chrome-tip-body");
  const btn = document.getElementById("ios-chrome-tip-dismiss");
  if (body) body.innerHTML = strings.bodyHtml;
  if (btn) btn.textContent = strings.ok;
  tip.dataset.i18nApplied = "1";
  tip.lang = locale;
}

function dismissIosChromeTip() {
  iosTipDismissed = true;
  const tip = document.getElementById("ios-chrome-tip");
  if (tip) tip.hidden = true;
  try {
    localStorage.setItem(IOS_TIP_KEY, "1");
  } catch {
    /* private mode — session flag still keeps it closed */
  }
}

function maybeShowIosChromeTip() {
  const tip = document.getElementById("ios-chrome-tip");
  if (!tip) return;
  applyIosChromeTipI18n();
  if (iosTipDismissed || isStandaloneDisplay() || !isLikelyIosSafari() || !isMobilePlay()) {
    tip.hidden = true;
    return;
  }
  try {
    if (localStorage.getItem(IOS_TIP_KEY) === "1") {
      iosTipDismissed = true;
      tip.hidden = true;
      return;
    }
  } catch {
    /* ignore */
  }
  // Most painful in landscape where the URL bar eats vertical space → width.
  const landscape =
    typeof window.matchMedia === "function" &&
    window.matchMedia("(orientation: landscape)").matches;
  tip.hidden = !landscape;
}

const CRANK_SWIPE_TIP_KEY = "zipper.crankSwipeTip.v1";

/** Session flag so a failed localStorage write still keeps the tip dismissed. */
let crankSwipeTipDismissed = false;

/** Menu "How to crank?" forces the tip open until OK (ignores auto gates / prior dismiss). */
let crankSwipeTipForced = false;

/**
 * Same locale set as the iOS chrome tip (en/zh/hi/es/fr).
 * Explains the host-only center-strip vertical swipe → crank.
 */
const CRANK_SWIPE_TIP_I18N = {
  en: {
    bodyHtml:
      "While aiming, swipe <strong>up or down</strong> in the <strong>center</strong> of the screen to turn the crank and preview enemy moves.",
    ok: "OK",
  },
  zh: {
    bodyHtml:
      "瞄准时，在屏幕<strong>中央</strong><strong>上下滑动</strong>即可转动曲柄，预览敌人行动。",
    ok: "好",
  },
  hi: {
    bodyHtml:
      "निशाना लगाते समय दुश्मनों की चाल देखने के लिए स्क्रीन के <strong>बीच</strong> में <strong>ऊपर-नीचे</strong> स्वाइप करें (क्रैंक)।",
    ok: "ठीक",
  },
  es: {
    bodyHtml:
      "Al apuntar, desliza <strong>arriba o abajo</strong> en el <strong>centro</strong> de la pantalla para girar la manivela y previsualizar los movimientos enemigos.",
    ok: "OK",
  },
  fr: {
    bodyHtml:
      "En visant, glisse <strong>verticalement</strong> au <strong>centre</strong> de l’écran pour tourner la manivelle et prévisualiser les déplacements ennemis.",
    ok: "OK",
  },
};

function pickCrankSwipeTipLocale() {
  const tags = [];
  try {
    if (Array.isArray(navigator.languages)) tags.push(...navigator.languages);
  } catch {
    /* ignore */
  }
  if (navigator.language) tags.push(navigator.language);
  for (const raw of tags) {
    if (!raw || typeof raw !== "string") continue;
    const tag = raw.toLowerCase();
    const primary = tag.split("-")[0];
    if (CRANK_SWIPE_TIP_I18N[tag]) return tag;
    if (CRANK_SWIPE_TIP_I18N[primary]) return primary;
  }
  return "en";
}

function applyCrankSwipeTipI18n() {
  const tip = document.getElementById("crank-swipe-tip");
  if (!tip || tip.dataset.i18nApplied === "1") return;
  const locale = pickCrankSwipeTipLocale();
  const strings = CRANK_SWIPE_TIP_I18N[locale] || CRANK_SWIPE_TIP_I18N.en;
  const body = document.getElementById("crank-swipe-tip-body");
  const btn = document.getElementById("crank-swipe-tip-dismiss");
  if (body) body.innerHTML = strings.bodyHtml;
  if (btn) btn.textContent = strings.ok;
  tip.dataset.i18nApplied = "1";
  tip.lang = locale;
}

function dismissCrankSwipeTip() {
  crankSwipeTipForced = false;
  crankSwipeTipDismissed = true;
  const tip = document.getElementById("crank-swipe-tip");
  if (tip) tip.hidden = true;
  try {
    localStorage.setItem(CRANK_SWIPE_TIP_KEY, "1");
  } catch {
    /* private mode — session flag still keeps it closed */
  }
}

/** Reopen the tip from the host menu (works after a prior dismiss). */
function showCrankSwipeTipFromMenu() {
  const tip = document.getElementById("crank-swipe-tip");
  if (!tip) return;
  applyCrankSwipeTipI18n();
  crankSwipeTipForced = true;
  tip.hidden = false;
}

/**
 * Mobile Down pad overlaps outdoor chest (145,176). Clear its backdrop blur
 * only while that chest is still present in the current room.
 * @param {{ shouldClearTouchDownBlur?: () => boolean }} app
 */
function syncTouchDownBlurClear(app) {
  if (!isMobilePlay()) {
    document.body.classList.remove("touch-down-clear");
    return;
  }
  let clear = false;
  try {
    clear =
      typeof app?.shouldClearTouchDownBlur === "function" &&
      app.shouldClearTouchDownBlur();
  } catch {
    clear = false;
  }
  document.body.classList.toggle("touch-down-clear", !!clear);
}

/**
 * Auto-show once when mobile-play + in-game crankhint gates are true.
 * Menu force-show stays visible until OK. Persist dismiss in localStorage.
 * @param {{ shouldShowCrankHint?: () => boolean }} app
 */
function maybeShowCrankSwipeTip(app) {
  const tip = document.getElementById("crank-swipe-tip");
  if (!tip) return;
  applyCrankSwipeTipI18n();
  if (crankSwipeTipForced) {
    tip.hidden = false;
    return;
  }
  if (crankSwipeTipDismissed || !isMobilePlay()) {
    tip.hidden = true;
    return;
  }
  try {
    if (localStorage.getItem(CRANK_SWIPE_TIP_KEY) === "1") {
      crankSwipeTipDismissed = true;
      tip.hidden = true;
      return;
    }
  } catch {
    /* ignore */
  }
  let show = false;
  try {
    show = typeof app?.shouldShowCrankHint === "function" && app.shouldShowCrankHint();
  } catch {
    show = false;
  }
  tip.hidden = !show;
}

/**
 * Mobile play: hide chrome, contain-scale the 400×240 canvas, show touch overlay.
 * Portrait and landscape both supported via CSS orientation rules.
 * Layout class is applied synchronously; fullscreen is best-effort and never blocks UI.
 */
function syncMobileLayoutSwitch() {
  const sw = document.getElementById("mobile-layout-switch");
  if (sw) sw.checked = isMobilePlay();
}

function syncIntegerScaleSwitch() {
  const sw = document.getElementById("integer-scale-switch");
  if (sw) sw.checked = integerScaleOnly;
}

function syncSfxVolumeSlider(pct = readSfxVolumePct()) {
  const slider = document.getElementById("sfx-volume");
  const label = document.getElementById("sfx-volume-value");
  const n = Math.max(0, Math.min(100, Math.round(pct)));
  if (slider) {
    slider.value = String(n);
    slider.setAttribute("aria-valuenow", String(n));
  }
  if (label) label.textContent = `${n}%`;
}

function applySfxVolume(app, pct, { persist = true } = {}) {
  const n = Math.max(0, Math.min(100, Math.round(pct)));
  syncSfxVolumeSlider(n);
  if (persist) writeSfxVolumePct(n);
  try {
    app.setVolume(n / 100);
  } catch (err) {
    console.warn("setVolume failed", err);
  }
}

function enterMobilePlay({ requestFs = true, persist = true } = {}) {
  document.body.classList.remove("immersive");
  document.body.classList.add("mobile-play");
  const overlay = document.getElementById("touch-overlay");
  if (overlay) overlay.setAttribute("aria-hidden", "false");
  syncViewportLayout();
  syncStageZoomUi();
  maybeShowIosChromeTip();
  syncMobileLayoutSwitch();
  if (persist) writeMobileLayoutPref(true);
  if (requestFs) {
    void requestBrowserFullscreen();
    tryCollapseSafariChrome();
  }
}

function exitMobilePlay({ persist = true } = {}) {
  document.body.classList.remove("mobile-play", "touch-down-clear");
  const overlay = document.getElementById("touch-overlay");
  if (overlay) overlay.setAttribute("aria-hidden", "true");
  const tip = document.getElementById("ios-chrome-tip");
  if (tip) tip.hidden = true;
  syncMobileLayoutSwitch();
  syncCanvasDisplaySize();
  if (persist) writeMobileLayoutPref(false);
  void exitBrowserFullscreen();
}

/**
 * Toggle immersive play (desktop F key): browser fullscreen + hide chrome +
 * scale the 400×240 canvas to fill the screen (aspect preserved). On a phone
 * this routes into mobile-play so the touch overlay stays available.
 */
async function toggleImmersive() {
  if (isMobilePlay()) {
    exitMobilePlay();
    return;
  }
  if (prefersMobilePlay()) {
    enterMobilePlay({ requestFs: true });
    return;
  }
  const entering = !document.body.classList.contains("immersive");
  if (entering) {
    document.body.classList.add("immersive");
    void requestBrowserFullscreen();
  } else {
    document.body.classList.remove("immersive");
    void exitBrowserFullscreen();
  }
  syncCanvasDisplaySize();
}

async function main() {
  // Enter touch layout ASAP on phones so loading isn't stuck behind desktop chrome.
  // Don't persist here — boot follows URL / saved switch / device heuristic only.
  if (prefersMobilePlay()) {
    enterMobilePlay({ requestFs: false, persist: false });
  }
  syncMobileLayoutSwitch();
  syncIntegerScaleSwitch();

  const canvas = document.getElementById("screen");
  const ctx = canvas.getContext("2d", { alpha: false });
  ctx.imageSmoothingEnabled = false;

  setStatus("Loading wasm…");
  await init();
  // Lua `main.lua`: random_seed = playdate.getSecondsSinceEpoch()
  // (seconds since 2000-01-01 UTC). Kept for the whole session; death does
  // not re-roll. Force via `#seed=N` (optionally with `#god&seed=N`).
  // Query `?seed=` is ignored — see docs/url-fragment-seed.md.
  // Mid-run resume: if `zipper.save.v1` has `loadsave===1`, that seed wins
  // over `#seed=` (same as Playdate reading `saved_game.seed` first).
  const PLAYDATE_EPOCH = 946684800;
  const resumeRaw = readMidRunSaveRaw();
  const resumeCandidate = parseMidRunSave(resumeRaw);
  const hashSeed = urlSeedFromHash();
  let randomSeed;
  if (resumeCandidate && resumeCandidate.loadsave === 1) {
    randomSeed = (resumeCandidate.seed >>> 0);
    if (hashSeed != null && hashSeed !== randomSeed) {
      console.info(
        "zipper resume: ignoring #seed=",
        hashSeed,
        "(save seed",
        randomSeed,
        "wins)",
      );
    }
  } else if (hashSeed != null) {
    randomSeed = hashSeed;
  } else {
    randomSeed = (Math.floor(Date.now() / 1000) - PLAYDATE_EPOCH) >>> 0;
  }
  const app = new ZipperApp(randomSeed);
  console.info("zipper random_seed:", app.randomSeed());
  // Host menu SFX volume (localStorage; default 60%). Applied before first play.
  applySfxVolume(app, readSfxVolumePct(), { persist: false });

  canvas.width = app.screenWidth();
  canvas.height = app.screenHeight();
  initStageZoomControls();
  syncCanvasDisplaySize();
  if (canvas.parentElement && typeof ResizeObserver !== "undefined") {
    new ResizeObserver(() => syncCanvasDisplaySize()).observe(canvas.parentElement);
  }

  // BYOA: restore the user's own Zipper.pdx from IndexedDB, else wait for a
  // drop/pick. lib/byoa-boot.js installs the worldmap, dialog scripts, intro
  // music, Sho.mid and every named texture/sound the core expects.
  // Best effort: ask the browser not to evict the extracted pdx under storage
  // pressure (may be denied; harmless either way).
  if (navigator.storage && typeof navigator.storage.persist === "function") {
    navigator.storage
      .persist()
      .then((granted) =>
        console.info(`zipper-rust: persistent storage ${granted ? "granted" : "not granted"}`),
      )
      .catch(() => {});
  }
  const portVersion = app.portVersion();
  let assetsReady = false;
  try {
    assetsReady = await tryRestoreFromCache(app, portVersion, setStatus);
  } catch (err) {
    console.warn("zipper-rust: cache restore failed", err);
    assetsReady = false;
  }
  if (!assetsReady) {
    await waitForByoaIngest(app, portVersion, setStatus);
  }
  const clearAssetsRow = document.getElementById("host-menu-clear-assets-row");
  if (clearAssetsRow) clearAssetsRow.hidden = false;
  byoaReady = true;

  // Local high scores (Catalog stand-in) — independent of mid-run save.
  const hsRaw = readHighscoresRaw();
  if (hsRaw && typeof app.applyHighscores === "function") {
    try {
      app.applyHighscores(hsRaw);
      console.info("zipper highscores: restored local board");
    } catch (err) {
      console.warn("zipper highscores apply failed", err);
    }
  }

  // Mid-run resume after worldmap + dialogs (`check_for_save`).
  if (resumeRaw && resumeCandidate && resumeCandidate.loadsave === 1) {
    try {
      const ok = app.applySave(resumeRaw);
      if (ok) {
        console.info(
          "zipper resume: restored mid-run save (seed",
          app.randomSeed(),
          ")",
        );
        flushSaveIfDirty(app);
      } else {
        console.info("zipper resume: applySave rejected blob");
      }
    } catch (err) {
      console.warn("zipper resume applySave failed", err);
    }
  }

  // Optional tools: host forwards the fragment; only a god-featured wasm enables
  // anything and may return menu HTML to inject. Normal builds always get null.
  let extraMenuUi = null;
  try {
    if (typeof app.applyUrlFragment === "function") {
      extraMenuUi = app.applyUrlFragment(window.location.hash || "");
    }
  } catch (err) {
    console.warn("zipper applyUrlFragment failed", err);
    extraMenuUi = null;
  }

  const logChestSpawnIfDevTools = () => {
    if (typeof app.devToolsActive !== "function" || !app.devToolsActive()) return;
    if (typeof app.takeChestSpawn !== "function") return;
    const chest = app.takeChestSpawn();
    if (chest === undefined || chest === null) return;
    if (chest.length >= 2) {
      console.info(`zipper chest_pos: ${chest[0]},${chest[1]}`);
    } else {
      console.info("zipper chest_pos: (none)");
    }
  };

  if (
    extraMenuUi &&
    typeof app.devToolsActive === "function" &&
    app.devToolsActive()
  ) {
    // applyUrlFragment already enabled tools (defaults on); overlay saved prefs.
    if (typeof app.setInvulnerable === "function") {
      app.setInvulnerable(readDtInvulnPref());
    }
    if (typeof app.setCheatKey === "function") {
      app.setCheatKey(readDtKeyPref());
    }
    logChestSpawnIfDevTools();
  }

  // Every texture/sound below is installed by lib/byoa-boot.js from the user's
  // own Zipper.pdx (cache restore or fresh ingest) — see lib/asset-manifest.js.
  console.info(
    `zipper sfx: ${app.sfxReadyCount()} ready, ${app.sfxPendingCount()} pending (unlock on first key/click)`,
  );

  let binds = loadBinds();
  let keyMap = buildKeyMap(binds);
  let listeningAction = null;
  const down = new Set();

  // Assigned once the crank slider is wired; Shift+arrows drive hold-repeat below.
  let applyCrank = (_deg) => {};
  /** Degrees per ghost step (`handle_crank_ghosts` threshold is 15°). */
  const CRANK_KEY_DEG = 15;
  /** Hold-repeat tick: 30 Hz (50% faster than aim-path D-pad's 20 Hz / 50 ms). */
  const CRANK_HOLD_TICK_MS = 1000 / 30;
  /** First auto-step on the 5th tick (~167 ms), same count as D-pad but at 30 Hz. */
  const CRANK_HOLD_REPEAT_AFTER = 4;
  /** Physical Shift+arrow hold for keyboard crank (ignores slow OS key-repeat). */
  let crankHold = null; // { dir: 1|-1, held: number, accum: number, last: number, raf: number }
  const crankArrowDown = { ArrowUp: false, ArrowDown: false };
  let crankShiftDown = false;

  function crankStep(dir) {
    const crankEl = document.getElementById("crank");
    const next = (Number(crankEl?.value || 0) + dir * CRANK_KEY_DEG + 360) % 360;
    applyCrank(next);
  }

  function stopCrankHold() {
    if (crankHold?.raf != null) cancelAnimationFrame(crankHold.raf);
    crankHold = null;
  }

  function startCrankHold(dir) {
    stopCrankHold();
    // If ↑/↓ was already held for move, drop it so we don't walk while cranking.
    for (const d of ["up", "down"]) {
      if (down.has(d)) {
        down.delete(d);
        app.setButton(d, false);
      }
    }
    app.ensureAudio();
    crankStep(dir);
    crankHold = { dir, held: 0, accum: 0, last: performance.now(), raf: 0 };
    const loop = (now) => {
      if (!crankHold) return;
      crankHold.accum += now - crankHold.last;
      crankHold.last = now;
      while (crankHold.accum >= CRANK_HOLD_TICK_MS) {
        crankHold.accum -= CRANK_HOLD_TICK_MS;
        const v = crankHold.held;
        if (v >= 0) crankHold.held = v + 1;
        if (v >= CRANK_HOLD_REPEAT_AFTER) crankStep(crankHold.dir);
      }
      crankHold.raf = requestAnimationFrame(loop);
    };
    crankHold.raf = requestAnimationFrame(loop);
  }

  function syncCrankHoldFromKeys() {
    if (!crankShiftDown) {
      stopCrankHold();
      return;
    }
    const wantDown = crankArrowDown.ArrowDown;
    const wantUp = crankArrowDown.ArrowUp;
    if (wantDown === wantUp) {
      // Neither, or both — don't crank.
      stopCrankHold();
      return;
    }
    const dir = wantDown ? 1 : -1;
    if (!crankHold || crankHold.dir !== dir) startCrankHold(dir);
  }

  const listEl = document.getElementById("keybind-list");
  const resetBtn = document.getElementById("keybinds-reset");

  function renderKeybinds() {
    listEl.replaceChildren();
    for (const { id, label } of ACTIONS) {
      const li = document.createElement("li");
      const name = document.createElement("span");
      name.className = "action";
      name.textContent = label;
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "bind-btn";
      btn.dataset.action = id;
      btn.textContent = displayKey(binds[id]);
      if (listeningAction === id) btn.classList.add("listening");
      btn.addEventListener("click", () => {
        listeningAction = id;
        renderKeybinds();
      });
      li.append(name, btn);
      listEl.append(li);
    }
    // Fixed host bindings (not remappable).
    const addFixed = (label, keys, title) => {
      const li = document.createElement("li");
      li.className = "keybind-fixed";
      const name = document.createElement("span");
      name.className = "action";
      name.textContent = label;
      const keysEl = document.createElement("span");
      keysEl.className = "bind-fixed";
      keysEl.textContent = keys;
      if (title) keysEl.title = title;
      li.append(name, keysEl);
      listEl.append(li);
    };
    addFixed(
      "Menu",
      "Esc",
      "Esc opens / closes the host menu. In the menu: D-pad moves, A selects, B backs out.",
    );
    addFixed(
      "Crank",
      "Shift+↑ / Shift+↓",
      "Hold Shift+↑/↓ to crank (same 20 Hz repeat as aiming). Mouse wheel still works.",
    );
  }

  function applyBinds(next) {
    binds = next;
    keyMap = buildKeyMap(binds);
    saveBinds(binds);
    listeningAction = null;
    renderKeybinds();
  }

  resetBtn.addEventListener("click", () => {
    applyBinds({ ...DEFAULT_BINDS });
  });

  renderKeybinds();
  // Layout-aware labels (IJKL + A/B). Falls back to language heuristic.
  void refreshLayoutLabels().then((ok) => {
    if (ok) renderKeybinds();
  });
  if (navigator.keyboard && typeof navigator.keyboard.addEventListener === "function") {
    navigator.keyboard.addEventListener("layoutchange", () => {
      void refreshLayoutLabels().then(() => renderKeybinds());
    });
  }

  const touchOverlay = document.getElementById("touch-overlay");
  const mobileToggle = document.getElementById("mobile-play-toggle");
  const mobileLayoutSwitch = document.getElementById("mobile-layout-switch");

  // Leaving browser fullscreen (OS UI / F toggle) should also drop immersive
  // chrome. On iOS there is often no Fullscreen API — the CSS class alone still
  // drives the layout. Esc opens the host menu and no longer exits immersive.
  document.addEventListener("fullscreenchange", () => {
    if (!document.fullscreenElement) {
      document.body.classList.remove("immersive");
      // Keep mobile-play unless the user leaves via the burger switch / F.
    }
    syncCanvasDisplaySize();
  });

  mobileToggle?.addEventListener("click", (e) => {
    e.preventDefault();
    enterMobilePlay({ requestFs: true });
  });

  mobileLayoutSwitch?.addEventListener("change", () => {
    if (mobileLayoutSwitch.checked) {
      enterMobilePlay({ requestFs: true });
    } else {
      exitMobilePlay();
    }
  });

  const integerScaleSwitch = document.getElementById("integer-scale-switch");
  integerScaleSwitch?.addEventListener("change", () => {
    integerScaleOnly = !!integerScaleSwitch.checked;
    writeIntegerScalePref(integerScaleOnly);
    syncCanvasDisplaySize();
    syncIntegerScaleSwitch();
  });
  syncIntegerScaleSwitch();

  const sfxVolumeSlider = document.getElementById("sfx-volume");
  /** Last volume % that played the aiming-slide preview (skip duplicate change events). */
  let lastSfxVolumePreviewPct = readSfxVolumePct();
  const onSfxVolumeInput = () => {
    if (!sfxVolumeSlider) return;
    const pct = Number.parseInt(sfxVolumeSlider.value, 10) || 0;
    applySfxVolume(app, pct);
    app.ensureAudio();
    // Aiming selector tick (`Sounds/select`) so the slider level is audible.
    if (pct !== lastSfxVolumePreviewPct) {
      lastSfxVolumePreviewPct = pct;
      try {
        app.playSfx("select");
      } catch (err) {
        console.warn("playSfx(select) failed", err);
      }
    }
  };
  sfxVolumeSlider?.addEventListener("input", onSfxVolumeInput);
  sfxVolumeSlider?.addEventListener("change", onSfxVolumeInput);

  // Dismiss on pointerup (not only click): iOS Safari often never fires click
  // when a parent touchstart called preventDefault (see blockIosLoupe).
  const iosTipDismiss = document.getElementById("ios-chrome-tip-dismiss");
  const onIosTipDismiss = (e) => {
    e.preventDefault();
    e.stopPropagation();
    dismissIosChromeTip();
  };
  iosTipDismiss?.addEventListener("pointerup", onIosTipDismiss);
  iosTipDismiss?.addEventListener("click", onIosTipDismiss);
  // Stop loupe-blocking touchstart from reaching the overlay while on the tip.
  document.getElementById("ios-chrome-tip")?.addEventListener(
    "touchstart",
    (e) => e.stopPropagation(),
    { passive: true, capture: true },
  );

  const crankSwipeTipDismiss = document.getElementById("crank-swipe-tip-dismiss");
  const onCrankSwipeTipDismiss = (e) => {
    e.preventDefault();
    e.stopPropagation();
    dismissCrankSwipeTip();
  };
  crankSwipeTipDismiss?.addEventListener("pointerup", onCrankSwipeTipDismiss);
  crankSwipeTipDismiss?.addEventListener("click", onCrankSwipeTipDismiss);
  document.getElementById("crank-swipe-tip")?.addEventListener(
    "touchstart",
    (e) => e.stopPropagation(),
    { passive: true, capture: true },
  );

  // Keep layout glued to the visible viewport (Safari chrome show/hide).
  syncViewportLayout();
  window.visualViewport?.addEventListener("resize", syncViewportLayout);
  window.visualViewport?.addEventListener("scroll", syncViewportLayout);
  window.addEventListener("resize", syncViewportLayout);
  window.addEventListener("orientationchange", () => {
    // iOS fires this before the visual viewport settles.
    setTimeout(() => {
      syncViewportLayout();
      maybeShowIosChromeTip();
      tryCollapseSafariChrome();
    }, 120);
  });
  // Retina / external-display DPR changes (move window between screens).
  const bindDprListener = () => {
    const mq = window.matchMedia(
      `(resolution: ${window.devicePixelRatio || 1}dppx)`,
    );
    const onDpr = () => {
      syncCanvasDisplaySize();
      bindDprListener();
    };
    if (typeof mq.addEventListener === "function") {
      mq.addEventListener("change", onDpr, { once: true });
    } else if (typeof mq.addListener === "function") {
      mq.addListener(function legacyDpr() {
        mq.removeListener(legacyDpr);
        onDpr();
      });
    }
  };
  bindDprListener();

  // First user gesture: try collapsing Safari chrome + request fullscreen where allowed.
  const onFirstGesture = () => {
    app.ensureAudio();
    if (!isMobilePlay()) return;
    tryCollapseSafariChrome();
    void requestBrowserFullscreen();
    syncViewportLayout();
  };
  window.addEventListener("pointerdown", onFirstGesture, { once: true, passive: true });

  // Safari suspends AudioContext after background / screen lock. Resume on return
  // and on the next gesture; clear held one-shots when leaving so we don't dump
  // a backlog of swooshes when the context wakes up.
  const resumeAudio = () => {
    try {
      app.ensureAudio();
    } catch (err) {
      console.warn("ensureAudio failed", err);
    }
  };
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") {
      flushTerminateSave(app);
      try {
        app.onAudioSuspend();
      } catch (err) {
        console.warn("onAudioSuspend failed", err);
      }
    } else {
      resumeAudio();
    }
  });
  window.addEventListener("pagehide", () => {
    flushTerminateSave(app);
  });
  window.addEventListener("pageshow", resumeAudio);
  window.addEventListener("focus", resumeAudio);

  // Re-assert after asset load (CDP / late media-query updates, or desktop→phone).
  if (prefersMobilePlay() && !isMobilePlay()) {
    enterMobilePlay({ requestFs: false, persist: false });
  } else if (isMobilePlay()) {
    syncViewportLayout();
    syncMobileLayoutSwitch();
    maybeShowIosChromeTip();
  }

  // Orientation flips (portrait ↔ landscape) only need CSS; keep class sticky.
  window.matchMedia("(orientation: landscape)").addEventListener("change", () => {
    if (prefersMobilePlay() && !isMobilePlay()) {
      enterMobilePlay({ requestFs: false, persist: false });
    }
    syncViewportLayout();
    syncMobileLayoutSwitch();
    maybeShowIosChromeTip();
  });

  // Mobile top crank pill removed (unused in gameplay; desktop slider + wheel remain).

  // Keyboard — binds use physical `code` (IJKL move; A/B = KeyX/KeyZ → AZERTY X/W).
  window.addEventListener("keydown", (e) => {
    if (noteLayoutFromEvent(e)) renderKeybinds();

    // Track Shift / arrows for hold-to-crank (even during Boot / listening).
    if (e.key === "Shift") crankShiftDown = true;
    if (e.code === "ArrowUp" || e.code === "ArrowDown") {
      crankArrowDown[e.code] = true;
    }

    // Rebind capture: eat the key, don't send it to the game.
    if (listeningAction) {
      e.preventDefault();
      if (e.code === "Escape" || e.key === "Escape") {
        listeningAction = null;
        renderKeybinds();
        return;
      }
      // Don't bind pure modifiers, or physical F (reserved for fullscreen).
      if (
        e.key === "Shift" ||
        e.key === "Control" ||
        e.key === "Alt" ||
        e.key === "Meta" ||
        e.code === "KeyF"
      ) {
        return;
      }
      const code = e.code || normalizeCode(e.key);
      if (!code) return;
      // Drop this physical key from any other action so one key → one remappable action.
      const next = { ...binds };
      for (const { id } of ACTIONS) {
        if (next[id] === code) next[id] = binds[listeningAction];
      }
      next[listeningAction] = code;
      applyBinds(next);
      return;
    }

    // Host QoL: physical F toggles browser fullscreen + immersive screen fit.
    if (e.code === "KeyF" && !e.repeat && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault();
      void toggleImmersive();
      return;
    }

    // Esc toggles the host menu (no longer B / immersive-exit).
    if (e.code === "Escape" || e.key === "Escape") {
      e.preventDefault();
      if (e.repeat) return;
      if (app.gameState() === "Boot") {
        app.ensureAudio();
        app.skipBoot();
      }
      toggleHostMenu();
      return;
    }

    const btn = actionFromEvent(keyMap, e);

    // While the shell menu is open, D-pad / A / B drive the menu — not the game.
    if (hostMenuOpen) {
      if (
        btn === "up" ||
        btn === "down" ||
        btn === "left" ||
        btn === "right" ||
        btn === "a" ||
        btn === "b"
      ) {
        e.preventDefault();
        if (e.repeat && (btn === "a" || btn === "b")) return;
        handleHostMenuButton(btn);
        return;
      }
      // Swallow other game keys so they don't leak under the dialog.
      if (btn) {
        e.preventDefault();
        return;
      }
    }

    // Host splash card: any key skips white intro → first room; idle auto-start is 2s.
    if (app.gameState() === "Boot") {
      if (
        e.key === "Shift" ||
        e.key === "Control" ||
        e.key === "Alt" ||
        e.key === "Meta" ||
        e.repeat
      ) {
        return;
      }
      e.preventDefault();
      app.ensureAudio();
      app.skipBoot();
      return;
    }
    // Shift+↑/↓ → crank with aim-path hold-repeat (20 Hz after ~250 ms).
    // Ignore OS key-repeat; our RAF loop drives continuous steps while held.
    // Mouse wheel over the canvas still works. Arrows without Shift still move.
    if (
      e.shiftKey &&
      !e.metaKey &&
      !e.ctrlKey &&
      !e.altKey &&
      (e.code === "ArrowUp" || e.code === "ArrowDown")
    ) {
      e.preventDefault();
      if (e.repeat) return;
      syncCrankHoldFromKeys();
      return;
    }
    // Shift alone while an arrow is already down also starts hold-crank.
    if (e.key === "Shift" && !e.metaKey && !e.ctrlKey && !e.altKey && !e.repeat) {
      if (crankArrowDown.ArrowUp || crankArrowDown.ArrowDown) {
        e.preventDefault();
        syncCrankHoldFromKeys();
        return;
      }
    }
    if (!btn) return;
    e.preventDefault();
    if (down.has(btn)) return;
    down.add(btn);
    app.ensureAudio();
    app.setButton(btn, true);
  });
  window.addEventListener("keyup", (e) => {
    if (e.key === "Shift") crankShiftDown = false;
    if (e.code === "ArrowUp" || e.code === "ArrowDown") {
      crankArrowDown[e.code] = false;
    }
    // Keep / stop hold-crank from the live modifier+arrow state.
    if (
      e.key === "Shift" ||
      e.code === "ArrowUp" ||
      e.code === "ArrowDown"
    ) {
      syncCrankHoldFromKeys();
    }

    if (listeningAction) {
      e.preventDefault();
      return;
    }
    // Don't let a remapped physical F leak into game on keyup.
    if (e.code === "KeyF" && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault();
      return;
    }
    if (e.code === "Escape" || e.key === "Escape") {
      e.preventDefault();
      return;
    }
    // Shift+arrows only cranked — never pressed a D-pad button.
    if (e.code === "ArrowUp" || e.code === "ArrowDown") {
      if (e.shiftKey || crankHold) {
        e.preventDefault();
        return;
      }
    }
    const btn = actionFromEvent(keyMap, e);
    if (!btn) return;
    e.preventDefault();
    // Menu ate the matching keydown — don't release a game button we never pressed.
    if (hostMenuOpen) return;
    down.delete(btn);
    app.setButton(btn, false);
  });
  window.addEventListener("blur", () => {
    crankShiftDown = false;
    crankArrowDown.ArrowUp = false;
    crankArrowDown.ArrowDown = false;
    stopCrankHold();
  });

  // ---- Host shell menu (burger / Esc). Expandable list of shell actions. ----
  const hostMenu = document.getElementById("host-menu");
  const hostMenuStatus = document.getElementById("host-menu-status");
  const hostMenuVersion = document.getElementById("host-menu-version");
  const hostMenuMain = document.getElementById("host-menu-main");
  const hostMenuExtraSlot = document.getElementById("host-menu-extra-slot");
  const hostMenuExtraPanelSlot = document.getElementById(
    "host-menu-extra-panel-slot",
  );
  const hostMenuTitle = document.getElementById("host-menu-title");
  /** Optional tools UI from wasm `applyUrlFragment` (null on normal builds). */
  let hostMenuExtraRow = null;
  let hostMenuExtraPanel = null;
  let invulnSwitch = null;
  let keySwitch = null;
  let teleX = null;
  let teleY = null;
  const devTools =
    typeof app.devToolsActive === "function" && !!app.devToolsActive();

  /** Inject tools markup returned by wasm into the neutral menu slots. */
  const injectExtraMenuUi = (html) => {
    if (!html || !hostMenuExtraSlot || !hostMenuExtraPanelSlot) return;
    const wrap = document.createElement("div");
    wrap.innerHTML = html;
    const row = wrap.querySelector("#host-menu-extra-row");
    const panel = wrap.querySelector("#host-menu-extra-panel");
    if (row) {
      hostMenuExtraSlot.replaceWith(row);
      hostMenuExtraRow = row;
    }
    if (panel) {
      hostMenuExtraPanelSlot.replaceWith(panel);
      hostMenuExtraPanel = panel;
    }
    invulnSwitch = document.getElementById("god-invuln-switch");
    keySwitch = document.getElementById("god-key-switch");
    teleX = document.getElementById("god-tele-x");
    teleY = document.getElementById("god-tele-y");
  };
  if (extraMenuUi) injectExtraMenuUi(extraMenuUi);

  if (hostMenuVersion) {
    try {
      hostMenuVersion.textContent = app.portVersion();
    } catch {
      hostMenuVersion.textContent = "";
    }
  }
  let hostMenuOpen = false;
  let hostMenuView = "main"; // "main" | "extra"
  /** Focused `<li>` inside the visible host-menu list (D-pad / A / B). */
  let hostMenuFocusLi = null;

  const setHostMenuStatus = (text, kind = "") => {
    if (!hostMenuStatus) return;
    if (!text) {
      hostMenuStatus.hidden = true;
      hostMenuStatus.textContent = "";
      hostMenuStatus.classList.remove("is-error", "is-ok");
      return;
    }
    hostMenuStatus.hidden = false;
    hostMenuStatus.textContent = text;
    hostMenuStatus.classList.toggle("is-error", kind === "error");
    hostMenuStatus.classList.toggle("is-ok", kind === "ok");
  };

  const syncExtraToolsRow = () => {
    if (!hostMenuExtraRow) return;
    hostMenuExtraRow.hidden = !devTools;
  };

  const syncExtraToolSwitches = () => {
    if (!devTools) return;
    if (invulnSwitch && typeof app.invulnerable === "function") {
      invulnSwitch.checked = !!app.invulnerable();
      invulnSwitch.disabled = false;
    }
    if (keySwitch && typeof app.cheatKey === "function") {
      keySwitch.checked = !!app.cheatKey();
      keySwitch.disabled = false;
    }
  };

  const prefillTeleport = () => {
    if (typeof app.playerTile !== "function") return;
    try {
      const tile = app.playerTile();
      if (tile && tile.length >= 2) {
        if (teleX) teleX.value = String(tile[0]);
        if (teleY) teleY.value = String(tile[1]);
      }
    } catch (err) {
      console.warn("playerTile failed", err);
    }
  };

  /** Worldmap-viewer clipboard `"x,y"` (optional quotes / spaces). */
  const parseTeleportPaste = (text) => {
    const raw = String(text ?? "")
      .trim()
      .replace(/^["']|["']$/g, "");
    const m = raw.match(/^(\d+)\s*,\s*(\d+)$/);
    if (!m) return null;
    return { x: m[1], y: m[2] };
  };

  const applyTeleportPaste = (text, { fromMenu = false } = {}) => {
    if (!devTools || typeof app.teleportTo !== "function") return false;
    const parsed = parseTeleportPaste(text);
    if (!parsed) return false;
    if (teleX) teleX.value = parsed.x;
    if (teleY) teleY.value = parsed.y;
    const x = Number.parseInt(parsed.x, 10);
    const y = Number.parseInt(parsed.y, 10);
    app.ensureAudio();
    const ok = app.teleportTo(x, y);
    if (!ok) {
      if (fromMenu) {
        setHostMenuStatus("Teleport failed (in-bounds tile required)", "error");
      } else {
        console.warn(`zipper teleport failed: ${x},${y}`);
      }
      return true;
    }
    if (fromMenu) {
      closeHostMenu();
    } else {
      console.info(`zipper teleported to ${x},${y}`);
    }
    return true;
  };

  const activeHostMenuList = () => {
    if (
      hostMenuView === "extra" &&
      hostMenuExtraPanel &&
      !hostMenuExtraPanel.hidden
    ) {
      return hostMenuExtraPanel;
    }
    return hostMenuMain;
  };

  const hostMenuFocusItems = () => {
    const list = activeHostMenuList();
    if (!list) return [];
    return [...list.querySelectorAll(":scope > li")].filter((li) => {
      if (li.hidden) return false;
      // Skip fully collapsed rows (e.g. optional tools when inactive).
      if (li.getClientRects().length === 0) return false;
      return true;
    });
  };

  const clearHostMenuFocus = () => {
    if (hostMenuFocusLi) {
      hostMenuFocusLi.classList.remove("is-focused");
      hostMenuFocusLi = null;
    }
  };

  const setHostMenuFocus = (li) => {
    if (!li) return;
    if (hostMenuFocusLi === li) {
      li.classList.add("is-focused");
      return;
    }
    if (hostMenuFocusLi) hostMenuFocusLi.classList.remove("is-focused");
    hostMenuFocusLi = li;
    li.classList.add("is-focused");
    // Keep the focused row in view inside the scrollable panel.
    try {
      li.scrollIntoView({ block: "nearest" });
    } catch {
      /* ignore */
    }
  };

  const ensureHostMenuFocus = () => {
    const items = hostMenuFocusItems();
    if (items.length === 0) {
      clearHostMenuFocus();
      return;
    }
    if (hostMenuFocusLi && items.includes(hostMenuFocusLi)) {
      hostMenuFocusLi.classList.add("is-focused");
      return;
    }
    setHostMenuFocus(items[0]);
  };

  const moveHostMenuFocus = (delta) => {
    const items = hostMenuFocusItems();
    if (items.length === 0) return;
    let idx = items.indexOf(hostMenuFocusLi);
    if (idx < 0) idx = 0;
    else idx = (idx + delta + items.length) % items.length;
    setHostMenuFocus(items[idx]);
  };

  const focusedHostMenuControl = () => {
    const li = hostMenuFocusLi;
    if (!li) return null;
    const checkbox = li.querySelector('input[type="checkbox"]');
    if (checkbox && !checkbox.disabled) return { kind: "checkbox", el: checkbox };
    const range = li.querySelector('input[type="range"]');
    if (range && !range.disabled) return { kind: "range", el: range };
    const go = li.querySelector("[data-menu-action='god-teleport']");
    if (go) return { kind: "teleport", el: go };
    const actionBtn = li.querySelector("button[data-menu-action]");
    if (actionBtn && !actionBtn.disabled) {
      return { kind: "action", el: actionBtn, action: actionBtn.getAttribute("data-menu-action") };
    }
    const link = li.querySelector("a.host-menu-link");
    if (link) return { kind: "link", el: link };
    return null;
  };

  const nudgeHostMenuValue = (dir) => {
    // dir: -1 left, +1 right
    const ctrl = focusedHostMenuControl();
    if (!ctrl) return;
    if (ctrl.kind === "checkbox") {
      ctrl.el.checked = !ctrl.el.checked;
      ctrl.el.dispatchEvent(new Event("change", { bubbles: true }));
      return;
    }
    if (ctrl.kind === "range") {
      const step = Number.parseFloat(ctrl.el.step) || 1;
      const min = Number.parseFloat(ctrl.el.min);
      const max = Number.parseFloat(ctrl.el.max);
      let next = Number.parseFloat(ctrl.el.value) + dir * step * 5;
      if (Number.isFinite(min)) next = Math.max(min, next);
      if (Number.isFinite(max)) next = Math.min(max, next);
      ctrl.el.value = String(next);
      ctrl.el.dispatchEvent(new Event("input", { bubbles: true }));
      return;
    }
  };

  /** Hold-repeat for on-screen L/R on the volume slider (mobile pads are one-shot otherwise). */
  const MENU_VOL_HOLD_TICK_MS = 1000 / 20; // 20 Hz, same cadence as aim D-pad
  const MENU_VOL_HOLD_REPEAT_AFTER = 4; // ~200 ms before auto-steps
  let menuVolumeHold = null; // { dir, held, accum, last, raf }

  const stopMenuVolumeHold = () => {
    if (menuVolumeHold?.raf != null) cancelAnimationFrame(menuVolumeHold.raf);
    menuVolumeHold = null;
  };

  const startMenuVolumeHold = (dir) => {
    stopMenuVolumeHold();
    nudgeHostMenuValue(dir);
    menuVolumeHold = {
      dir,
      held: 0,
      accum: 0,
      last: performance.now(),
      raf: 0,
    };
    const loop = (now) => {
      if (!menuVolumeHold || !hostMenuOpen) {
        stopMenuVolumeHold();
        return;
      }
      // Drop hold if focus left the volume row.
      const ctrl = focusedHostMenuControl();
      if (!ctrl || ctrl.kind !== "range") {
        stopMenuVolumeHold();
        return;
      }
      menuVolumeHold.accum += now - menuVolumeHold.last;
      menuVolumeHold.last = now;
      while (menuVolumeHold.accum >= MENU_VOL_HOLD_TICK_MS) {
        menuVolumeHold.accum -= MENU_VOL_HOLD_TICK_MS;
        const v = menuVolumeHold.held;
        if (v >= 0) menuVolumeHold.held = v + 1;
        if (v >= MENU_VOL_HOLD_REPEAT_AFTER) {
          nudgeHostMenuValue(menuVolumeHold.dir);
        }
      }
      menuVolumeHold.raf = requestAnimationFrame(loop);
    };
    menuVolumeHold.raf = requestAnimationFrame(loop);
  };

  const activateHostMenuFocus = () => {
    const ctrl = focusedHostMenuControl();
    if (!ctrl) return;
    if (ctrl.kind === "checkbox") {
      ctrl.el.checked = !ctrl.el.checked;
      ctrl.el.dispatchEvent(new Event("change", { bubbles: true }));
      return;
    }
    if (ctrl.kind === "range") {
      // A on volume does nothing special — Left/Right adjust.
      return;
    }
    if (ctrl.kind === "teleport") {
      runExtraTeleport();
      return;
    }
    if (ctrl.kind === "action") {
      runHostMenuAction(ctrl.action);
      return;
    }
    if (ctrl.kind === "link") {
      // Open in a new tab like a click; keep the menu open.
      try {
        ctrl.el.click();
      } catch {
        const href = ctrl.el.getAttribute("href");
        if (href) window.open(href, "_blank", "noopener,noreferrer");
      }
    }
  };

  const runHostMenuAction = (action) => {
    if (!action) return;
    if (action === "close") {
      closeHostMenu();
      return;
    }
    if (action === "god-tools") {
      showHostMenuExtra();
      return;
    }
    if (action === "god-back") {
      setHostMenuStatus("");
      showHostMenuMain();
      return;
    }
    if (action === "god-teleport") {
      runExtraTeleport();
      return;
    }
    if (action === "god-force-win") {
      if (typeof app.forceWin !== "function") return;
      closeHostMenu();
      app.ensureAudio();
      app.forceWin();
      return;
    }
    if (action === "seppuku") {
      closeHostMenu();
      app.ensureAudio();
      app.seppuku();
      flushSaveIfDirty(app);
      return;
    }
    if (action === "delete-save") {
      closeHostMenu();
      try {
        if (typeof app.deleteSaveMenu === "function") {
          app.deleteSaveMenu();
        } else if (typeof app.deleteSave === "function") {
          persistMidRunSave(app.deleteSave());
        }
        flushSaveIfDirty(app);
        // Same random_seed as this page load (1.10: seed only set at module load).
        // New seed without killing the PWA: **Delete save**, then **Reboot**.
        console.info(
          "zipper delete save: mid-run cleared; dialog counters reset (in-RAM); seed unchanged",
          app.randomSeed(),
        );
      } catch (err) {
        console.warn("zipper delete save failed", err);
      }
      return;
    }
    if (action === "reboot") {
      // Host-only stand-in for quitting the Playdate app: persist terminate
      // snapshot (if mid-run), then full reload. Resume + same seed when
      // loadsave==1. New epoch seed only after Delete save (or no save) and
      // no `#seed=` in the URL.
      closeHostMenu();
      flushTerminateSave(app);
      console.info("zipper reboot: reloading (like app quit)");
      window.location.reload();
      return;
    }
    if (action === "how-to-crank") {
      closeHostMenu();
      showCrankSwipeTipFromMenu();
      return;
    }
    if (action === "clear-assets") {
      // Drop the IndexedDB BYOA bundle, then reload back to the drop gate.
      closeHostMenu();
      void clearByoaBundle().then(() => {
        console.info("zipper-rust: saved assets cleared — reloading");
        window.location.reload();
      });
      return;
    }
  };

  /** D-pad / A / B while the host menu is open (keyboard or on-screen pads). */
  const handleHostMenuButton = (btn) => {
    if (!hostMenuOpen) return;
    if (btn === "up") {
      moveHostMenuFocus(-1);
      return;
    }
    if (btn === "down") {
      moveHostMenuFocus(1);
      return;
    }
    if (btn === "left") {
      nudgeHostMenuValue(-1);
      return;
    }
    if (btn === "right") {
      nudgeHostMenuValue(1);
      return;
    }
    if (btn === "a") {
      activateHostMenuFocus();
      return;
    }
    if (btn === "b") {
      if (hostMenuView === "extra") {
        setHostMenuStatus("");
        showHostMenuMain();
      } else {
        closeHostMenu();
      }
    }
  };

  const showHostMenuMain = () => {
    hostMenuView = "main";
    if (hostMenuMain) hostMenuMain.hidden = false;
    if (hostMenuExtraPanel) hostMenuExtraPanel.hidden = true;
    if (hostMenuTitle) hostMenuTitle.textContent = "Menu";
    syncExtraToolsRow();
    clearHostMenuFocus();
    ensureHostMenuFocus();
  };

  const showHostMenuExtra = () => {
    if (!devTools || !hostMenuExtraPanel) {
      showHostMenuMain();
      return;
    }
    hostMenuView = "extra";
    if (hostMenuMain) hostMenuMain.hidden = true;
    hostMenuExtraPanel.hidden = false;
    if (hostMenuTitle) {
      hostMenuTitle.textContent =
        hostMenuExtraPanel.getAttribute("data-menu-title") || "Menu";
    }
    syncExtraToolSwitches();
    prefillTeleport();
    clearHostMenuFocus();
    ensureHostMenuFocus();
  };

  const syncHostMenuOpenClass = () => {
    // Mobile: raise touch pads above the menu dimmer while open (see style.css).
    document.body.classList.toggle("host-menu-open", hostMenuOpen);
  };

  const closeHostMenu = () => {
    if (!hostMenu || !hostMenuOpen) return;
    stopMenuVolumeHold();
    hostMenu.hidden = true;
    hostMenuOpen = false;
    syncHostMenuOpenClass();
    clearHostMenuFocus();
    setHostMenuStatus("");
    // Reset to main view without re-focusing a hidden list.
    hostMenuView = "main";
    if (hostMenuMain) hostMenuMain.hidden = false;
    if (hostMenuExtraPanel) hostMenuExtraPanel.hidden = true;
    if (hostMenuTitle) hostMenuTitle.textContent = "Menu";
  };

  const openHostMenu = () => {
    if (!hostMenu) return;
    hostMenu.hidden = false;
    hostMenuOpen = true;
    syncHostMenuOpenClass();
    setHostMenuStatus("");
    showHostMenuMain();
    syncMobileLayoutSwitch();
    syncSfxVolumeSlider();
    app.ensureAudio();
    ensureHostMenuFocus();
  };

  const toggleHostMenu = () => {
    if (hostMenuOpen) closeHostMenu();
    else openHostMenu();
  };

  const runExtraTeleport = () => {
    if (!devTools || typeof app.teleportTo !== "function") return;
    const x = Number.parseInt(teleX?.value ?? "", 10);
    const y = Number.parseInt(teleY?.value ?? "", 10);
    if (!Number.isFinite(x) || !Number.isFinite(y)) {
      setHostMenuStatus("Enter integer x and y tile coords", "error");
      return;
    }
    app.ensureAudio();
    const ok = app.teleportTo(x, y);
    if (!ok) {
      setHostMenuStatus("Teleport failed (in-bounds tile required)", "error");
      return;
    }
    prefillTeleport();
    setHostMenuStatus(`Teleported to ${x},${y}`, "ok");
  };

  const applyInvulnSwitch = () => {
    if (!devTools || !invulnSwitch || typeof app.setInvulnerable !== "function") {
      return;
    }
    const on = !!invulnSwitch.checked;
    app.setInvulnerable(on);
    writeDtInvulnPref(on);
    setHostMenuStatus(on ? "Invulnerability on" : "Invulnerability off", "ok");
  };

  const applyKeySwitch = () => {
    if (!devTools || !keySwitch || typeof app.setCheatKey !== "function") {
      return;
    }
    const on = !!keySwitch.checked;
    app.setCheatKey(on);
    writeDtKeyPref(on);
    setHostMenuStatus(on ? "Castle key granted" : "Castle key cleared", "ok");
  };

  invulnSwitch?.addEventListener("change", applyInvulnSwitch);
  keySwitch?.addEventListener("change", applyKeySwitch);

  // Worldmap viewer copies `"x,y"` — paste teleports when optional tools are on:
  // tools submenu (fills fields + Go), or mid-play with the menu closed.
  document.addEventListener("paste", (e) => {
    if (!devTools) return;
    const inExtraTools = hostMenuOpen && hostMenuView === "extra";
    if (hostMenuOpen && !inExtraTools) return;

    // Don't steal paste from unrelated inputs (keybinds, volume, etc.).
    const ae = document.activeElement;
    const inTeleField = ae === teleX || ae === teleY;
    if (
      ae &&
      !inTeleField &&
      (ae.tagName === "INPUT" ||
        ae.tagName === "TEXTAREA" ||
        ae.tagName === "SELECT" ||
        ae.isContentEditable)
    ) {
      return;
    }

    const text = e.clipboardData?.getData("text/plain") ?? "";
    if (!parseTeleportPaste(text)) return;
    if (!applyTeleportPaste(text, { fromMenu: inExtraTools })) return;
    e.preventDefault();
  });

  // Pointer/mouse focus follows the row under the cursor.
  hostMenu?.addEventListener("pointerover", (e) => {
    if (!hostMenuOpen) return;
    const li = e.target.closest(".host-menu-list > li");
    if (!li || li.hidden) return;
    const list = activeHostMenuList();
    if (!list || !list.contains(li)) return;
    setHostMenuFocus(li);
  });

  hostMenu?.addEventListener("click", (e) => {
    const actionEl = e.target.closest("[data-menu-action]");
    if (!actionEl) return;
    const action = actionEl.getAttribute("data-menu-action");
    // Checkbox / range / number fields handle themselves; don't steal.
    if (
      action === "mobile-layout" ||
      action === "sfx-volume" ||
      action === "god-invuln" ||
      action === "god-key"
    ) {
      return;
    }
    e.preventDefault();
    e.stopPropagation();
    runHostMenuAction(action);
  });

  // On-screen buttons (desktop chrome + mobile touch overlay).
  // pointer capture keeps a held finger "pressed" even if it slides slightly.
  // Menu button toggles the host shell; D-pad / A / B navigate it when open.
  for (const el of document.querySelectorAll("[data-btn]")) {
    const name = el.getAttribute("data-btn");
    if (!name) continue;
    if (name === "menu") {
      // Desktop mouse synthesizes click after pointerup — listening to both
      // toggled the menu open then shut in the same gesture. Touch needs
      // pointerup because overlay touchstart preventDefault kills click.
      let ignoreNextClick = false;
      const openFromPointer = (e) => {
        e.preventDefault();
        e.stopPropagation();
        // Mouse: wait for click (one clean activation). Pen/touch: open now.
        if (e.pointerType === "mouse") return;
        ignoreNextClick = true;
        toggleHostMenu();
      };
      const openFromClick = (e) => {
        e.preventDefault();
        e.stopPropagation();
        if (ignoreNextClick) {
          ignoreNextClick = false;
          return;
        }
        toggleHostMenu();
      };
      el.addEventListener("pointerup", openFromPointer);
      el.addEventListener("click", openFromClick);
      continue;
    }
    const press = (e) => {
      e.preventDefault();
      e.stopPropagation();
      try {
        el.setPointerCapture(e.pointerId);
      } catch {
        /* ignore */
      }
      el.classList.add("active");
      app.ensureAudio();
      if (
        hostMenuOpen &&
        (name === "up" ||
          name === "down" ||
          name === "left" ||
          name === "right" ||
          name === "a" ||
          name === "b")
      ) {
        // Volume L/R: hold-repeat on mobile pads (keyboard already has OS repeat).
        if (
          (name === "left" || name === "right") &&
          isMobilePlay() &&
          focusedHostMenuControl()?.kind === "range"
        ) {
          startMenuVolumeHold(name === "left" ? -1 : 1);
          return;
        }
        // One-shot navigation; don't hold-feed the game.
        handleHostMenuButton(name);
        return;
      }
      app.setButton(name, true);
    };
    const release = (e) => {
      e.preventDefault();
      el.classList.remove("active");
      if (name === "left" || name === "right") stopMenuVolumeHold();
      if (hostMenuOpen) return;
      app.setButton(name, false);
    };
    el.addEventListener("pointerdown", press);
    el.addEventListener("pointerup", release);
    el.addEventListener("pointercancel", release);
    el.addEventListener("lostpointercapture", release);
  }

  // Crank slider (desktop) + center-band vertical swipe (mobile overlay)
  const crank = document.getElementById("crank");
  const crankDeg = document.getElementById("crank-deg");
  const crankStrip = document.getElementById("touch-crank-strip");
  applyCrank = (deg) => {
    const d = ((Number(deg) % 360) + 360) % 360;
    crank.value = String(Math.round(d));
    crankDeg.textContent = `${Math.round(d)}°`;
    if (crankStrip) crankStrip.setAttribute("aria-valuenow", String(Math.round(d)));
    app.setCrank(d, false);
  };
  crank.addEventListener("input", () => applyCrank(crank.value));

  // Host splash: click/tap the card → skip white intro, land in first room.
  // (Keyboard skip is handled in the keydown path; idle timeout is 2s → Title.)
  const skipBootFromPointer = (e) => {
    if (app.gameState() !== "Boot") return;
    e.preventDefault();
    app.ensureAudio();
    app.skipBoot();
  };
  canvas.addEventListener("pointerdown", skipBootFromPointer);
  touchOverlay?.addEventListener("pointerdown", (e) => {
    // Only when the gesture is on empty overlay chrome (not a d-pad / A/B pad).
    if (e.target !== touchOverlay) return;
    skipBootFromPointer(e);
  });

  // Wheel over canvas → crank
  canvas.addEventListener(
    "wheel",
    (e) => {
      e.preventDefault();
      const next = (Number(crank.value) + e.deltaY * 0.25 + e.deltaX * 0.25 + 360) % 360;
      applyCrank(next);
    },
    { passive: false },
  );

  // Mobile: vertical swipe in the center 25% strip → crank Δ (ghost preview).
  // Finger down → positive Δ (ghosts advance); finger up → rewind. ~2° per CSS px
  // so ~8 px ≈ one 15° ghost step. Short tap still skips Boot.
  // PX_PER_DEG: degrees per pixel of vertical travel.
  const CRANK_STRIP_DEG_PER_PX = 2;
  const CRANK_STRIP_TAP_SLOP_PX = 12;
  if (crankStrip) {
    let stripPtr = null;
    const endStrip = (e) => {
      if (!stripPtr || e.pointerId !== stripPtr.id) return;
      const traveled = Math.hypot(e.clientX - stripPtr.x0, e.clientY - stripPtr.y0);
      crankStrip.classList.remove("dragging");
      try {
        crankStrip.releasePointerCapture(e.pointerId);
      } catch {
        /* already released */
      }
      const wasTap = traveled < CRANK_STRIP_TAP_SLOP_PX;
      stripPtr = null;
      if (wasTap) skipBootFromPointer(e);
    };
    crankStrip.addEventListener("pointerdown", (e) => {
      if (!isMobilePlay()) return;
      if (e.pointerType === "mouse" && e.button !== 0) return;
      e.preventDefault();
      app.ensureAudio();
      stripPtr = { id: e.pointerId, y: e.clientY, x0: e.clientX, y0: e.clientY };
      crankStrip.classList.add("dragging");
      try {
        crankStrip.setPointerCapture(e.pointerId);
      } catch {
        /* capture optional */
      }
    });
    crankStrip.addEventListener("pointermove", (e) => {
      if (!stripPtr || e.pointerId !== stripPtr.id) return;
      e.preventDefault();
      const dy = e.clientY - stripPtr.y;
      if (dy === 0) return;
      stripPtr.y = e.clientY;
      // Screen +y is down; map finger-down to +crank (same sign as wheel deltaY).
      const next =
        (Number(crank.value) + dy * CRANK_STRIP_DEG_PER_PX + 360) % 360;
      applyCrank(next);
    });
    crankStrip.addEventListener("pointerup", endStrip);
    crankStrip.addEventListener("pointercancel", endStrip);
    crankStrip.addEventListener("lostpointercapture", (e) => {
      if (stripPtr && e.pointerId === stripPtr.id) {
        crankStrip.classList.remove("dragging");
        stripPtr = null;
      }
    });
  }


  // Block default gestures on the overlay so the page doesn't scroll/zoom mid-fight.
  // touchstart preventDefault is what stops iOS's double-tap-hold magnifying glass
  // on the pads / canvas (CSS -webkit-touch-callout alone is not enough there).
  // Skip the iOS chrome tip: preventDefault on its touchstart kills click synthesis
  // for the OK button on Safari, so the dialog could never be dismissed.
  const isInsideIosChromeTip = (t) =>
    t instanceof Element && !!t.closest("#ios-chrome-tip");
  const blockIosLoupe = (e) => {
    if (!isMobilePlay()) return;
    if (isInsideIosChromeTip(e.target)) return;
    e.preventDefault();
  };
  touchOverlay?.addEventListener("touchstart", blockIosLoupe, { passive: false });
  touchOverlay?.addEventListener(
    "touchmove",
    (e) => {
      if (!isMobilePlay()) return;
      if (isInsideIosChromeTip(e.target)) return;
      e.preventDefault();
    },
    { passive: false },
  );
  canvas.addEventListener("touchstart", blockIosLoupe, { passive: false });
  canvas.addEventListener(
    "touchmove",
    (e) => {
      if (isMobilePlay()) e.preventDefault();
    },
    { passive: false },
  );

  // Phone QoL: kill text-selection / callout / double-tap zoom / pinch-zoom.
  // CSS `user-select` + `touch-action` cover most browsers; these catch Safari leftovers.
  document.addEventListener("selectstart", (e) => e.preventDefault());
  document.addEventListener("contextmenu", (e) => e.preventDefault());
  document.addEventListener("gesturestart", (e) => e.preventDefault(), { passive: false });
  document.addEventListener("gesturechange", (e) => e.preventDefault(), { passive: false });
  document.addEventListener("gestureend", (e) => e.preventDefault(), { passive: false });
  document.addEventListener("dblclick", (e) => e.preventDefault(), { capture: true });

  let last = performance.now();
  let frames = 0;
  let fpsT = last;

  function tick(now) {
    const dt = Math.min(0.05, Math.max(0, (now - last) / 1000));
    last = now;
    try {
      app.frame(ctx, dt);
    } catch (err) {
      setStatus(`Frame error: ${err}`, true);
      console.error(err);
      return;
    }
    // Mid-run save: door exit / death / win dirty → localStorage (1.10 datastore).
    flushSaveIfDirty(app);
    // Local high scores after score-screen submit.
    flushHsIfDirty(app);
    // Optional tools: log each `chester:reset` when enabled.
    logChestSpawnIfDevTools();
    maybeShowCrankSwipeTip(app);
    syncTouchDownBlurClear(app);
    frames += 1;
    if (now - fpsT > 2000) {
      const fps = (frames * 1000) / (now - fpsT);
      frames = 0;
      fpsT = now;
      // Dev-only heartbeat — keep the page chrome clean.
      console.debug(
        `zipper frame: state=${app.gameState()} · ${fps.toFixed(0)} fps · dt=${(dt * 1000).toFixed(1)} ms`,
      );
    }
    requestAnimationFrame(tick);
  }

  setStatus("");
  requestAnimationFrame(tick);
}

function showByoaGate(show) {
  const gate = document.getElementById("byoa-gate");
  if (!gate) return;
  gate.hidden = !show;
  document.body.classList.toggle("byoa-waiting", !!show);
}

function setByoaError(msg) {
  const el = document.getElementById("byoa-error");
  if (!el) return;
  if (!msg) {
    el.hidden = true;
    el.textContent = "";
    return;
  }
  el.hidden = false;
  el.textContent = msg;
}

/**
 * Drive the BYOA progress bar. `done == null` hides it; `total == null` renders
 * an indeterminate (animated) bar for phases whose size is not yet known.
 * @param {number|null} done
 * @param {number|null} [total]
 */
function setByoaProgress(done, total) {
  const wrap = document.getElementById("byoa-progress");
  const fill = document.getElementById("byoa-progress-fill");
  if (!wrap || !fill) return;
  if (done == null) {
    wrap.hidden = true;
    wrap.classList.remove("is-indeterminate");
    fill.style.width = "0%";
    return;
  }
  wrap.hidden = false;
  if (total == null || total <= 0) {
    wrap.classList.add("is-indeterminate");
    fill.style.width = "100%";
  } else {
    wrap.classList.remove("is-indeterminate");
    fill.style.width = `${Math.min(100, Math.round((done / total) * 100))}%`;
  }
}

/**
 * Block until the user supplies a Zipper.pdx (zip or directory), then ingest + cache.
 * @param {*} app
 * @param {string} portVersion
 * @param {(msg: string, isError?: boolean) => void} setStatus
 */
function waitForByoaIngest(app, portVersion, setStatus) {
  return new Promise((resolve, reject) => {
    showByoaGate(true);
    setByoaError("");
    setStatus("");

    const drop = document.getElementById("byoa-drop");
    const zipInput = document.getElementById("byoa-zip-input");
    const dirInput = document.getElementById("byoa-dir-input");
    const pickerBtn = document.getElementById("byoa-picker-btn");
    let busy = false;

    const finishOk = () => {
      showByoaGate(false);
      setByoaError("");
      setStatus("Ready.");
      cleanup();
      resolve();
    };

    const runWithPathMap = async (pathMap) => {
      if (busy) return;
      busy = true;
      setByoaError("");
      try {
        await ingestPathMap(pathMap, app, portVersion, (msg, frac) => {
          setStatus(msg);
          if (frac) setByoaProgress(frac.done, frac.total);
        });
        setByoaProgress(null);
        console.info(
          `zipper-rust: ingested pdx (${app.sfxReadyCount()} sfx ready)`,
        );
        finishOk();
      } catch (err) {
        setByoaProgress(null);
        console.error(err);
        const msg = err && err.message ? err.message : String(err);
        setByoaError(msg);
        setStatus(msg, true);
        busy = false;
      }
    };

    // Folder phases report a file count; `total == null` is the scan pass.
    const onFolderProgress = (done, total) => {
      if (total == null) {
        setStatus(`Scanning folder… ${done} files`);
        setByoaProgress(0, null);
      } else {
        setStatus(`Reading folder… ${done}/${total}`);
        setByoaProgress(done, total);
      }
    };

    const handleZipFile = async (file) => {
      if (!file) return;
      setStatus(`Reading ${file.name}…`);
      setByoaProgress(0, null);
      const buf = new Uint8Array(await file.arrayBuffer());
      setStatus(`Unpacking ${file.name}…`);
      const pathMap = pathMapFromZip(buf);
      await runWithPathMap(pathMap);
    };

    const handleFileList = async (list) => {
      if (!list || !list.length) return;
      // Single .zip dropped via file input / OS drop of one archive.
      if (list.length === 1 && /\.zip$/i.test(list[0].name)) {
        await handleZipFile(list[0]);
        return;
      }
      setStatus("Reading folder…");
      const pathMap = await pathMapFromFileList(list, onFolderProgress);
      await runWithPathMap(pathMap);
    };

    const onDragOver = (e) => {
      e.preventDefault();
      drop?.classList.add("dragover");
    };
    const onDragLeave = () => drop?.classList.remove("dragover");
    // Drag-drop is desktop-only; on touch the zone must open the picker itself.
    const onDropClick = () => {
      zipInput?.click();
    };
    const onDropKey = (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        zipInput?.click();
      }
    };
    const onDrop = async (e) => {
      e.preventDefault();
      drop?.classList.remove("dragover");
      const dt = e.dataTransfer;
      if (!dt) return;
      // Prefer directory entries when present.
      const items = dt.items ? [...dt.items] : [];
      if (items.length && typeof items[0].webkitGetAsEntry === "function") {
        const entry = items[0].webkitGetAsEntry();
        if (entry && entry.isDirectory) {
          setStatus("Scanning folder…");
          const pathMap = await pathMapFromDirectoryEntry(entry, onFolderProgress);
          await runWithPathMap(pathMap);
          return;
        }
      }
      await handleFileList(dt.files);
    };

    const onZipChange = async () => {
      const f = zipInput?.files?.[0];
      if (f) await handleZipFile(f);
      if (zipInput) zipInput.value = "";
    };
    const onDirChange = async () => {
      if (dirInput?.files?.length) await handleFileList(dirInput.files);
      if (dirInput) dirInput.value = "";
    };
    const onPicker = async () => {
      try {
        const handle = await window.showDirectoryPicker();
        setStatus("Reading folder…");
        const pathMap = await pathMapFromDirectoryHandle(handle, onFolderProgress);
        await runWithPathMap(pathMap);
      } catch (err) {
        if (err && err.name === "AbortError") return;
        console.error(err);
        setByoaError(err.message || String(err));
      }
    };

    function cleanup() {
      drop?.removeEventListener("dragover", onDragOver);
      drop?.removeEventListener("dragleave", onDragLeave);
      drop?.removeEventListener("drop", onDrop);
      drop?.removeEventListener("click", onDropClick);
      drop?.removeEventListener("keydown", onDropKey);
      zipInput?.removeEventListener("change", onZipChange);
      dirInput?.removeEventListener("change", onDirChange);
      pickerBtn?.removeEventListener("click", onPicker);
    }

    if (pickerBtn && typeof window.showDirectoryPicker === "function") {
      pickerBtn.hidden = false;
    }

    drop?.addEventListener("dragover", onDragOver);
    drop?.addEventListener("dragleave", onDragLeave);
    drop?.addEventListener("drop", onDrop);
    drop?.addEventListener("click", onDropClick);
    drop?.addEventListener("keydown", onDropKey);
    zipInput?.addEventListener("change", onZipChange);
    dirInput?.addEventListener("change", onDirChange);
    pickerBtn?.addEventListener("click", onPicker);
  });
}

main().catch((err) => {
  console.error(err);
  setStatus(`Failed to start: ${err}`, true);
  // If boot failed before the gate was satisfied, keep the picker visible so
  // the user is not staring at a blank screen.
  if (!byoaReady) showByoaGate(true);
});
