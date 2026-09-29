/**
 * Build `ZMAP` bytes from a Tiled-like map object (same layout as `tools/worldmap_to_bin.py`).
 *
 * Layout (little-endian):
 *   magic "ZMAP" | u32 version=1 | u16 width | u16 height
 *   tiles:  u16[width*height]     // Outside Layer GIDs
 *   enemies:u8[width*height]      // Enemies layer, GIDs >511 already -= 512
 *   u32 n_exits
 *   each exit:
 *     i32 id, x, y, nx, ny, sx, sy, entrance, face, heal
 *     u8 reverse | u8 kind (0=exit,1=npc,2=other)
 *     u16 dialog_len | dialog utf-8
 *     u16 n_teleports | i32 teleports[n]
 *   u32 n_chests
 *   each chest: i32 x, y, gid
 */

export const ZMAP_W = 256;
export const ZMAP_H = 256;

function asArray(v) {
  if (!v) return [];
  if (Array.isArray(v)) return v;
  if (v && typeof v === "object" && Array.isArray(v._arr)) return v._arr;
  return [];
}

function layerByName(root, name) {
  const layers = asArray(root.layers);
  for (const layer of layers) {
    if (layer && layer.name === name) return layer;
  }
  throw new Error(`ZMAP: missing layer ${JSON.stringify(name)}`);
}

function tileLayer(root, name) {
  const layer = layerByName(root, name);
  const data = asArray(layer.data);
  if (data.length < ZMAP_W * ZMAP_H) {
    throw new Error(
      `ZMAP: ${name} has ${data.length} tiles, need ${ZMAP_W * ZMAP_H}`,
    );
  }
  return data.slice(0, ZMAP_W * ZMAP_H).map((x) => Number(x) | 0);
}

function objectLayer(root, name) {
  const layer = layerByName(root, name);
  return asArray(layer.objects).map((o) => {
    const props = o.properties && typeof o.properties === "object" ? o.properties : {};
    // Strip internal array marker if present
    const clean = { ...props };
    delete clean._arr;
    return {
      id: Number(o.id) | 0,
      type: o.type || "",
      x: Number(o.x) || 0,
      y: Number(o.y) || 0,
      gid: Number(o.gid) || 0,
      properties: clean,
    };
  });
}

function pi(props, key, d = 0) {
  const v = props[key];
  if (v === undefined || v === null || v === "") return d;
  const n = Number.parseInt(String(v), 10);
  return Number.isFinite(n) ? n : d;
}

function parseExit(e) {
  const p = e.properties;
  let teleports = [];
  if (p.teleport_id !== undefined && p.teleport_id !== null && p.teleport_id !== "") {
    teleports = String(p.teleport_id)
      .split(",")
      .map((t) => t.trim())
      .filter(Boolean)
      .map((t) => Number.parseInt(t, 10));
  }
  return {
    id: e.id,
    type: e.type || "exit",
    x: Math.trunc(e.x / 16 - 1),
    y: Math.trunc(e.y / 16 - 1),
    nx: pi(p, "center_n_x"),
    ny: pi(p, "center_n_y"),
    sx: pi(p, "center_s_x"),
    sy: pi(p, "center_s_y"),
    entrance: pi(p, "entrance", -1),
    face: pi(p, "face", -1),
    heal: pi(p, "heal", 0),
    dialog: String(p.dialog ?? ""),
    reverse: p.reverse ? 1 : 0,
    teleports,
  };
}

/**
 * @param {object} root Tiled map table (from LuaT eval or equivalent)
 * @returns {Uint8Array}
 */
export function encodeZmap(root) {
  const outside = tileLayer(root, "Outside Layer");
  const enemiesRaw = tileLayer(root, "Enemies");
  const enemiesU8 = enemiesRaw.map((v) => {
    let x = v > 511 ? v - 512 : v;
    if (x < 0) x = 0;
    if (x > 255) x = 255;
    return x;
  });
  const exits = objectLayer(root, "Exits").map(parseExit);
  const chests = objectLayer(root, "Chests").map((c) => ({
    x: Math.trunc(c.x / 16 - 1),
    y: Math.trunc(c.y / 16 - 1),
    gid: Number(c.gid) || 0,
  }));

  // Pre-size buffer roughly
  let dialogBytes = 0;
  let teleBytes = 0;
  for (const e of exits) {
    dialogBytes += new TextEncoder().encode(e.dialog).length;
    teleBytes += 4 * e.teleports.length;
  }
  const size =
    4 +
    4 +
    4 +
    2 * ZMAP_W * ZMAP_H +
    ZMAP_W * ZMAP_H +
    4 +
    exits.length * (40 + 2 + 2 + 2) +
    dialogBytes +
    teleBytes +
    4 +
    chests.length * 12;
  const buf = new ArrayBuffer(size);
  const view = new DataView(buf);
  const u8 = new Uint8Array(buf);
  let off = 0;
  u8[0] = 0x5a;
  u8[1] = 0x4d;
  u8[2] = 0x41;
  u8[3] = 0x50; // ZMAP
  off = 4;
  view.setUint32(off, 1, true);
  off += 4;
  view.setUint16(off, ZMAP_W, true);
  off += 2;
  view.setUint16(off, ZMAP_H, true);
  off += 2;
  for (const t of outside) {
    view.setUint16(off, t & 0xffff, true);
    off += 2;
  }
  u8.set(enemiesU8, off);
  off += enemiesU8.length;
  view.setUint32(off, exits.length, true);
  off += 4;
  const enc = new TextEncoder();
  for (const e of exits) {
    view.setInt32(off, e.id, true); off += 4;
    view.setInt32(off, e.x, true); off += 4;
    view.setInt32(off, e.y, true); off += 4;
    view.setInt32(off, e.nx, true); off += 4;
    view.setInt32(off, e.ny, true); off += 4;
    view.setInt32(off, e.sx, true); off += 4;
    view.setInt32(off, e.sy, true); off += 4;
    view.setInt32(off, e.entrance, true); off += 4;
    view.setInt32(off, e.face, true); off += 4;
    view.setInt32(off, e.heal, true); off += 4;
    const kind = e.type === "exit" ? 0 : e.type === "npc" ? 1 : 2;
    u8[off++] = e.reverse & 1;
    u8[off++] = kind;
    const dlg = enc.encode(e.dialog);
    view.setUint16(off, dlg.length, true);
    off += 2;
    u8.set(dlg, off);
    off += dlg.length;
    view.setUint16(off, e.teleports.length, true);
    off += 2;
    for (const t of e.teleports) {
      view.setInt32(off, t, true);
      off += 4;
    }
  }
  view.setUint32(off, chests.length, true);
  off += 4;
  for (const c of chests) {
    view.setInt32(off, c.x, true); off += 4;
    view.setInt32(off, c.y, true); off += 4;
    view.setInt32(off, c.gid, true); off += 4;
  }
  return u8.subarray(0, off);
}
