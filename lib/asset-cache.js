/**
 * IndexedDB cache for BYOA payloads (worldmap + named asset blobs).
 * localStorage stays for tiny prefs only — never store pdx bytes there.
 */

const DB_NAME = "zipperust-byoa";
const DB_VERSION = 1;
const STORE = "bundle";
const BUNDLE_KEY = "current";

/**
 * Bump when the manifest / loader contract changes (asset set, worldmap or
 * dialog derivation) so old caches invalidate. The port version is **not** a
 * cache key: the bundle holds raw pdx-derived bytes that don't change with the
 * wasm, so gating on it forced a re-upload on every shell/wasm build.
 */
export const BYOA_CACHE_SCHEMA = 4;

/**
 * @typedef {{
 *   schema: number,
 *   portVersion: string,
 *   pdxVersion: string|null,
 *   savedAt: number,
 *   worldmap: Uint8Array,
 *   dialogs: string[][],
 *   introLuac: Uint8Array|null,
 *   creditsLuac: Uint8Array|null,
 *   midi: Uint8Array|null,
 *   assets: Record<string, { kind: string, data: Uint8Array }>,
 * }} ByoaBundle
 */

/** @returns {Promise<IDBDatabase>} */
function openDb() {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, DB_VERSION);
    req.onerror = () => reject(req.error || new Error("IndexedDB open failed"));
    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains(STORE)) {
        db.createObjectStore(STORE);
      }
    };
    req.onsuccess = () => resolve(req.result);
  });
}

/**
 * @template T
 * @param {IDBRequest<T>} req
 * @returns {Promise<T>}
 */
function idbReq(req) {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error || new Error("IndexedDB request failed"));
  });
}

/**
 * @param {string} expectedPortVersion
 * @returns {Promise<ByoaBundle|null>}
 */
export async function loadByoaBundle(expectedPortVersion) {
  try {
    const db = await openDb();
    try {
      const tx = db.transaction(STORE, "readonly");
      const store = tx.objectStore(STORE);
      /** @type {ByoaBundle|undefined} */
      const bundle = await idbReq(store.get(BUNDLE_KEY));
      if (!bundle || typeof bundle !== "object") return null;
      if (bundle.schema !== BYOA_CACHE_SCHEMA) {
        console.info("zipper-rust: IDB schema mismatch — ignoring cache");
        return null;
      }
      if (expectedPortVersion && bundle.portVersion !== expectedPortVersion) {
        // Informational only — do NOT discard. The bundle is raw, version-
        // independent pdx bytes; correctness is keyed on BYOA_CACHE_SCHEMA.
        console.info(
          `zipper-rust: IDB bundle from port ${bundle.portVersion} (now ${expectedPortVersion}) — keeping`,
        );
      }
      if (!(bundle.worldmap instanceof Uint8Array) || bundle.worldmap.length < 12) {
        return null;
      }
      if (!Array.isArray(bundle.dialogs) || bundle.dialogs.length === 0) {
        console.info("zipper-rust: IDB bundle missing dialogs — ignoring cache");
        return null;
      }
      if (!bundle.assets || typeof bundle.assets !== "object") return null;
      return bundle;
    } finally {
      db.close();
    }
  } catch (err) {
    console.warn("zipper-rust: IDB load failed", err);
    return null;
  }
}

/**
 * @param {ByoaBundle} bundle
 * @returns {Promise<void>}
 */
export async function saveByoaBundle(bundle) {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readwrite");
    const store = tx.objectStore(STORE);
    await idbReq(store.put(bundle, BUNDLE_KEY));
    await new Promise((resolve, reject) => {
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(tx.error || new Error("IDB save failed"));
      tx.onabort = () => reject(tx.error || new Error("IDB save aborted"));
    });
  } finally {
    db.close();
  }
}

/** @returns {Promise<void>} */
export async function clearByoaBundle() {
  try {
    const db = await openDb();
    try {
      const tx = db.transaction(STORE, "readwrite");
      const store = tx.objectStore(STORE);
      await idbReq(store.delete(BUNDLE_KEY));
      await new Promise((resolve, reject) => {
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error || new Error("IDB clear failed"));
        tx.onabort = () => reject(tx.error || new Error("IDB clear aborted"));
      });
    } finally {
      db.close();
    }
  } catch (err) {
    console.warn("zipper-rust: IDB clear failed", err);
  }
}

/** @returns {Promise<boolean>} */
export async function hasByoaBundle() {
  const db = await openDb();
  try {
    const tx = db.transaction(STORE, "readonly");
    const store = tx.objectStore(STORE);
    const v = await idbReq(store.get(BUNDLE_KEY));
    return !!v;
  } catch {
    return false;
  } finally {
    db.close();
  }
}
