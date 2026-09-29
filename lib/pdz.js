/**
 * Playdate `.pdz` container reader.
 * Format: https://github.com/cranksters/playdate-reverse-engineering/blob/main/formats/pdz.md
 * Port of `tools/pdz_extract.py` (Zipper 1.10 main.pdz is not encrypted).
 *
 * Browser: use `extractPdzAsync` (DecompressionStream).
 * Node: either API works (`node:zlib` for sync).
 */

const PDZ_IDENT = "Playdate PDZ";

const FILE_TYPES = {
  1: "luac",
  2: "pdi",
  3: "pdt",
  4: "pdv",
  5: "pda",
  6: "pds",
  7: "pft",
};

function readCString(u8, off) {
  let end = off;
  while (end < u8.length && u8[end] !== 0) end++;
  if (end >= u8.length) throw new Error("PDZ: truncated name");
  const name = new TextDecoder("utf-8").decode(u8.subarray(off, end));
  return { name, next: end + 1 };
}

async function loadNodeInflateSync() {
  const { inflateSync } = await import("node:zlib");
  return (bytes) => new Uint8Array(inflateSync(Buffer.from(bytes)));
}

async function inflateZlibAsync(bytes) {
  if (typeof DecompressionStream !== "undefined") {
    const ds = new DecompressionStream("deflate");
    const stream = new Blob([bytes]).stream().pipeThrough(ds);
    const buf = await new Response(stream).arrayBuffer();
    return new Uint8Array(buf);
  }
  const inflate = await loadNodeInflateSync();
  return inflate(bytes);
}

function parseEntries(u8) {
  const magic = new TextDecoder("ascii").decode(u8.subarray(0, 12));
  if (magic !== PDZ_IDENT) {
    throw new Error(`PDZ: bad magic ${JSON.stringify(magic)}`);
  }
  const view = new DataView(u8.buffer, u8.byteOffset, u8.byteLength);
  const flags = view.getUint32(12, true);
  if (flags & 0x40000000) {
    throw new Error("PDZ is encrypted (Catalog DRM) — cannot extract");
  }

  /** @type {{name: string, type: string, compressed: boolean, blob: Uint8Array}[]} */
  const raw = [];
  let ptr = 16;
  const n = u8.length;
  while (ptr + 4 <= n) {
    const head = view.getUint32(ptr, true);
    ptr += 4;
    const eflags = head & 0xff;
    let entryLen = (head >>> 8) & 0xffffff;
    const isCompressed = (eflags >>> 7) & 1;
    const ftype = FILE_TYPES[eflags & 0x7f] || `type${eflags & 0x7f}`;
    const { name, next } = readCString(u8, ptr);
    ptr = next;
    ptr = (ptr + 3) & ~3;

    if (ftype === "pda") {
      if (ptr + 4 > n) break;
      ptr += 4;
      entryLen -= 4;
    }

    if (isCompressed) {
      if (ptr + 4 > n) break;
      ptr += 4;
      entryLen -= 4;
    }

    const blob = u8.subarray(ptr, ptr + entryLen);
    ptr += entryLen;
    raw.push({
      name,
      type: ftype,
      compressed: Boolean(isCompressed),
      blob: new Uint8Array(blob),
    });
  }
  return raw;
}

/**
 * Async extract — preferred everywhere (browser + Node).
 * @param {Uint8Array} data
 */
export async function extractPdzAsync(data) {
  const u8 = data instanceof Uint8Array ? data : new Uint8Array(data);
  const raw = parseEntries(u8);
  const out = [];
  for (const e of raw) {
    out.push({
      name: e.name,
      type: e.type,
      compressed: e.compressed,
      data: e.compressed ? await inflateZlibAsync(e.blob) : e.blob,
    });
  }
  return out;
}

/**
 * Sync extract for Node tests. Throws in browsers without a sync zlib.
 * @param {Uint8Array} data
 * @param {(bytes: Uint8Array) => Uint8Array} [inflate]
 */
export function extractPdz(data, inflate) {
  const u8 = data instanceof Uint8Array ? data : new Uint8Array(data);
  if (!inflate) {
    throw new Error(
      "extractPdz requires an inflate(bytes) callback in sync mode; use extractPdzAsync instead",
    );
  }
  return parseEntries(u8).map((e) => ({
    name: e.name,
    type: e.type,
    compressed: e.compressed,
    data: e.compressed ? inflate(e.blob) : e.blob,
  }));
}

/** @param {{name: string, type: string, data: Uint8Array}[]} entries */
export function getWorldmapLuac(entries) {
  const hit = entries.find(
    (e) =>
      e.type === "luac" &&
      (e.name === "worldmap" || e.name.endsWith("/worldmap")),
  );
  if (!hit) throw new Error('PDZ: missing "worldmap" luac entry');
  return hit.data;
}

/** @param {{name: string, type: string, data: Uint8Array}[]} entries */
export function getScriptLuac(entries) {
  const hit = entries.find(
    (e) =>
      e.type === "luac" &&
      (e.name === "script" || e.name.endsWith("/script")),
  );
  if (!hit) throw new Error('PDZ: missing "script" luac entry');
  return hit.data;
}

/** `main` boot chunk — pass bytes to `app.loadCreditsFromLuac` (Rust extract). */
/** @param {{name: string, type: string, data: Uint8Array}[]} entries */
export function getMainLuac(entries) {
  const hit = entries.find(
    (e) =>
      e.type === "luac" && (e.name === "main" || e.name.endsWith("/main")),
  );
  if (!hit) throw new Error('PDZ: missing "main" luac entry');
  return hit.data;
}

/** Globals boot chunk — pass bytes to `app.loadIntroMusicFromLuac` (Rust extract). */
/** @param {{name: string, type: string, data: Uint8Array}[]} entries */
export function getGlobalsLuac(entries) {
  const hit = entries.find(
    (e) =>
      e.type === "luac" &&
      (e.name === "Globals" || e.name.endsWith("/Globals")),
  );
  if (!hit) throw new Error('PDZ: missing "Globals" luac entry');
  return hit.data;
}
