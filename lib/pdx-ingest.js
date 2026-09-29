/**
 * Build a normalized path → Uint8Array map from a Zipper.pdx zip or directory.
 */

import { unzipSync, strFromU8 } from "./fflate.js";

/**
 * @param {string} path
 * @returns {string}
 */
export function normalizePdxPath(path) {
  let p = String(path || "").replace(/\\/g, "/");
  // Strip leading ./ and collapse //
  p = p.replace(/^\.\/+/, "").replace(/\/{2,}/g, "/");
  // If the zip wrapped a single top folder (Zipper.pdx/… or foo/Zipper.pdx/…),
  // peel until Images|Sounds|Fonts|main.pdz|pdxinfo sit at the root.
  const markers = ["Images/", "Sounds/", "Fonts/", "main.pdz", "pdxinfo"];
  for (;;) {
    const hit = markers.some((m) => p === m || p.startsWith(m));
    if (hit) break;
    const slash = p.indexOf("/");
    if (slash < 0) break;
    p = p.slice(slash + 1);
  }
  return p;
}

/**
 * @param {Uint8Array} zipBytes
 * @returns {Map<string, Uint8Array>}
 */
export function pathMapFromZip(zipBytes) {
  const files = unzipSync(zipBytes);
  /** @type {Map<string, Uint8Array>} */
  const map = new Map();
  for (const [rawName, data] of Object.entries(files)) {
    if (!data || rawName.endsWith("/")) continue;
    const path = normalizePdxPath(rawName);
    if (!path || path.endsWith("/")) continue;
    map.set(path, data instanceof Uint8Array ? data : new Uint8Array(data));
  }
  return map;
}

/**
 * Bounded-concurrency read pool. A folder drop exposes one File per entry;
 * reading them strictly one-by-one serializes per-file I/O latency and makes a
 * folder drop far slower than a single (already-in-memory) zip. Each item is
 * `{ rel, getFile() }`, so `entry.file()` / `handle.getFile()` also run inside
 * the pool instead of in a separate serial pass.
 */
const READ_CONCURRENCY = 8;

function fileFromEntry(entry) {
  return new Promise((resolve, reject) => entry.file(resolve, reject));
}

/**
 * @param {{rel: string, getFile: () => Promise<File>}[]} items
 * @param {(done: number, total: number) => void} [onProgress]
 * @returns {Promise<Map<string, Uint8Array>>}
 */
async function readIntoMap(items, onProgress) {
  /** @type {Map<string, Uint8Array>} */
  const map = new Map();
  const total = items.length;
  let done = 0;
  let next = 0;
  onProgress?.(0, total);
  const worker = async () => {
    for (;;) {
      const i = next++;
      if (i >= total) return;
      const item = items[i];
      const path = normalizePdxPath(item.rel);
      if (path && !path.endsWith("/")) {
        try {
          const file = await item.getFile();
          map.set(path, new Uint8Array(await file.arrayBuffer()));
        } catch (err) {
          console.warn(`zipper-rust: failed to read ${item.rel}`, err);
        }
      }
      done++;
      onProgress?.(done, total);
    }
  };
  await Promise.all(
    Array.from({ length: Math.min(READ_CONCURRENCY, total) }, worker),
  );
  return map;
}

/**
 * @param {FileList|File[]} fileList  from <input webkitdirectory> or DataTransfer
 * @param {(done: number, total: number) => void} [onProgress]
 * @returns {Promise<Map<string, Uint8Array>>}
 */
export async function pathMapFromFileList(fileList, onProgress) {
  const items = Array.from(fileList || []).map((file) => ({
    rel: file.webkitRelativePath || file.name,
    getFile: async () => file,
  }));
  return readIntoMap(items, onProgress);
}

/**
 * Collect file entries under a dropped directory (`webkitGetAsEntry`) into a
 * flat list, then read them with the shared pool. Collection only touches
 * directory metadata; it runs first so the total is known for a determinate
 * progress bar.
 * @param {FileSystemDirectoryEntry} dirEntry
 * @param {(done: number, total: number|null) => void} [onProgress]
 * @returns {Promise<Map<string, Uint8Array>>}
 */
export async function pathMapFromDirectoryEntry(dirEntry, onProgress) {
  /** @type {{rel: string, getFile: () => Promise<File>}[]} */
  const items = [];
  async function collect(dir, prefix) {
    const reader = dir.createReader();
    for (;;) {
      const batch = await new Promise((resolve, reject) =>
        reader.readEntries(resolve, reject),
      );
      if (!batch.length) break;
      for (const ent of batch) {
        const rel = prefix ? `${prefix}/${ent.name}` : ent.name;
        if (ent.isFile) {
          items.push({ rel, getFile: () => fileFromEntry(ent) });
          onProgress?.(items.length, null);
        } else if (ent.isDirectory) {
          await collect(ent, rel);
        }
      }
    }
  }
  await collect(dirEntry, "");
  return readIntoMap(items, onProgress);
}

/**
 * Recursively read a File System Access API directory handle.
 * @param {FileSystemDirectoryHandle} dirHandle
 * @param {(done: number, total: number|null) => void} [onProgress]
 * @returns {Promise<Map<string, Uint8Array>>}
 */
export async function pathMapFromDirectoryHandle(dirHandle, onProgress) {
  /** @type {{rel: string, getFile: () => Promise<File>}[]} */
  const items = [];
  async function collect(dir, prefix) {
    for await (const [name, handle] of dir.entries()) {
      const rel = prefix ? `${prefix}/${name}` : name;
      if (handle.kind === "directory") {
        await collect(handle, rel);
      } else if (handle.kind === "file") {
        items.push({ rel, getFile: () => handle.getFile() });
        onProgress?.(items.length, null);
      }
    }
  }
  await collect(dirHandle, "");
  return readIntoMap(items, onProgress);
}

/**
 * @param {Map<string, Uint8Array>} map
 * @returns {{ ok: true } | { ok: false, reason: string }}
 */
export function validatePdxTree(map) {
  const hasMain = map.has("main.pdz");
  const hasImages = [...map.keys()].some((k) => k.startsWith("Images/"));
  const hasPdxinfo = map.has("pdxinfo");
  if (!hasMain) {
    return {
      ok: false,
      reason: 'Not a Zipper.pdx tree: missing main.pdz (zip the folder that contains it).',
    };
  }
  if (!hasImages) {
    return { ok: false, reason: "Not a Zipper.pdx tree: missing Images/." };
  }
  if (!hasPdxinfo) {
    console.warn("zipper-rust: pdxinfo missing — continuing");
  }
  return { ok: true };
}

/**
 * @param {Map<string, Uint8Array>} map
 * @param {string[]} candidates
 * @returns {Uint8Array|null}
 */
export function findBytes(map, candidates) {
  for (const c of candidates) {
    if (map.has(c)) return map.get(c);
  }
  // Case-insensitive fallback (some dumps differ).
  const lower = new Map([...map.keys()].map((k) => [k.toLowerCase(), k]));
  for (const c of candidates) {
    const real = lower.get(c.toLowerCase());
    if (real) return map.get(real);
  }
  return null;
}

/**
 * Read pdxversion from pdxinfo if present.
 * @param {Map<string, Uint8Array>} map
 * @returns {string|null}
 */
export function readPdxVersion(map) {
  const raw = map.get("pdxinfo");
  if (!raw) return null;
  const text = strFromU8(raw);
  const m = text.match(/pdxversion\s*=\s*(\d+)/i);
  return m ? m[1] : null;
}
