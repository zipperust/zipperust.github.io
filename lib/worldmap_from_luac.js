/**
 * Playdate LuaT (`\x1bLua\x54`) specialized evaluator for Zipper's `worldmap.luac`.
 *
 * The chunk is a pure Tiled `return { … }` table (no functions). We parse the
 * proto, run a minimal Lua 5.4-ish VM for table construction, and emit a plain
 * JS object suitable for `encodeZmap` in `./zmap.js`.
 *
 * Format reference: tools/vendor/unluac-src (Playdate fork) — not shipped.
 */

import { encodeZmap } from "./zmap.js";

class Reader {
  constructor(u8) {
    this.u8 = u8 instanceof Uint8Array ? u8 : new Uint8Array(u8);
    this.view = new DataView(this.u8.buffer, this.u8.byteOffset, this.u8.byteLength);
    this.i = 0;
  }
  u8b() {
    return this.u8[this.i++];
  }
  take(n) {
    const s = this.u8.subarray(this.i, this.i + n);
    this.i += n;
    return s;
  }
  i32() {
    const v = this.view.getInt32(this.i, true);
    this.i += 4;
    return v;
  }
  u32() {
    const v = this.view.getUint32(this.i, true);
    this.i += 4;
    return v;
  }
  f32() {
    const v = this.view.getFloat32(this.i, true);
    this.i += 4;
    return v;
  }
  /** BIntegerType54 — 7-bit big-endian chunks, stop when high bit set. */
  varint() {
    let x = 0;
    for (;;) {
      const b = this.u8b();
      x = (x << 7) | (b & 0x7f);
      if (b & 0x80) return x;
    }
  }
}

class LuaTable {
  constructor() {
    this.array = new Map(); // 1-based
    this.hash = new Map();
  }
  get(key) {
    if (typeof key === "number" && Number.isInteger(key) && key >= 1) {
      if (this.array.has(key)) return this.array.get(key);
    }
    return this.hash.has(key) ? this.hash.get(key) : undefined;
  }
  set(key, val) {
    if (typeof key === "number" && !Object.is(key, -0) && Number.isInteger(key) && key >= 1) {
      this.array.set(key, val);
      return;
    }
    this.hash.set(key, val);
  }
  toJs() {
    const hasHash = this.hash.size > 0;
    if (!hasHash) {
      if (this.array.size === 0) return {};
      const m = Math.max(...this.array.keys());
      const arr = new Array(m);
      for (let i = 1; i <= m; i++) arr[i - 1] = toJs(this.array.get(i));
      return arr;
    }
    const o = {};
    for (const [k, v] of this.hash) o[k] = toJs(v);
    // Rare mixed tables: expose numeric keys too
    for (const [k, v] of this.array) o[k] = toJs(v);
    return o;
  }
}

function toJs(v) {
  if (v instanceof LuaTable) return v.toJs();
  return v;
}

function parseString(r) {
  const size = r.varint();
  if (size === 0) return "";
  // LStringType54: size includes trailing NUL; payload is size-1 bytes (UTF-8).
  return new TextDecoder("utf-8").decode(r.take(size - 1));
}

function parseConstant(r) {
  const t = r.u8b();
  if (t === 0) return null;
  if (t === 1) return false;
  if (t === 17) return true; // 1 | VARIANT
  if (t === 3) return r.i32(); // INTEGER
  if (t === 19) return r.f32(); // FLOAT
  if (t === 4 || t === 20) return parseString(r); // SHORT / LONG string
  throw new Error(`LuaT: unknown constant type ${t} at ${r.i}`);
}

function parseFunction(r) {
  parseString(r); // source name
  r.varint(); // linedefined
  r.varint(); // lastlinedefined
  const nparams = r.u8b();
  const vararg = r.u8b();
  const maxstack = r.u8b();
  const ncode = r.varint();
  const code = new Array(ncode);
  for (let i = 0; i < ncode; i++) code[i] = r.u32();
  const nconst = r.varint();
  const constants = new Array(nconst);
  for (let i = 0; i < nconst; i++) constants[i] = parseConstant(r);
  const nups = r.varint();
  for (let i = 0; i < nups; i++) {
    r.u8b();
    r.u8b();
    r.u8b();
  }
  const nfuncs = r.varint();
  const funcs = new Array(nfuncs);
  for (let i = 0; i < nfuncs; i++) funcs[i] = parseFunction(r);
  // debug — skip
  const nlines = r.varint();
  r.take(nlines);
  const nabs = r.varint();
  for (let i = 0; i < nabs; i++) {
    r.varint();
    r.varint();
  }
  const nlocals = r.varint();
  for (let i = 0; i < nlocals; i++) {
    parseString(r);
    r.varint();
    r.varint();
  }
  const nupnames = r.varint();
  for (let i = 0; i < nupnames; i++) parseString(r);
  return { code, constants, maxstack, nparams, vararg, funcs };
}

/**
 * Parse a Playdate Lua 5.4 / LuaT chunk into the main function proto.
 * @param {Uint8Array} data
 */
export function parseLuac(data) {
  const r = new Reader(data);
  const sig = r.take(4);
  if (sig[0] !== 0x1b || sig[1] !== 0x4c || sig[2] !== 0x75 || sig[3] !== 0x61) {
    throw new Error("LuaT: bad signature");
  }
  if (r.u8b() !== 0x54) throw new Error("LuaT: expected Lua 5.4");
  if (r.u8b() !== 0) throw new Error("LuaT: unexpected format");
  const tail = r.take(6);
  const expect = [0x19, 0x93, 0x0d, 0x0a, 0x1a, 0x0a];
  for (let i = 0; i < 6; i++) {
    if (tail[i] !== expect[i]) throw new Error("LuaT: bad header tail");
  }
  const instr = r.u8b();
  const intSize = r.u8b();
  const numSize = r.u8b();
  if (instr !== 4 || intSize !== 4 || numSize !== 4) {
    throw new Error(
      `LuaT: unsupported sizes instr=${instr} int=${intSize} num=${numSize}`,
    );
  }
  if (r.i32() !== 0x5678) throw new Error("LuaT: bad LUAC_INT");
  if (Math.abs(r.f32() - 370.5) > 1e-4) throw new Error("LuaT: bad LUAC_NUM");
  r.u8b(); // main upvalue count
  const fn = parseFunction(r);
  if (r.i !== r.u8.length) {
    // tolerate trailing padding; worldmap consumes exactly
  }
  return fn;
}

const OP = (i) => i & 0x7f;
const A = (i) => (i >>> 7) & 0xff;
const B = (i) => (i >>> 16) & 0xff;
const C = (i) => (i >>> 24) & 0xff;
const kBit = (i) => (i >>> 15) & 1;
const Bx = (i) => (i >>> 15) & 0x1ffff;
const sBx = (i) => Bx(i) - 0xffff;

/**
 * Evaluate a data-only proto to a plain JS value (Tiled map root).
 * @param {{code: number[], constants: any[], maxstack: number}} fn
 */
export function evaluateTableChunk(fn) {
  const code = fn.code;
  const K = fn.constants;
  const R = new Array(fn.maxstack + 16).fill(undefined);
  let pc = 0;
  const n = code.length;

  while (pc < n) {
    const i = code[pc++];
    const op = OP(i);
    const a = A(i);

    if (op === 79) continue; // VARARGPREP

    if (op === 1) {
      // LOADI
      R[a] = sBx(i);
      continue;
    }
    if (op === 2) {
      // LOADF
      R[a] = sBx(i);
      continue;
    }
    if (op === 3) {
      // LOADK
      R[a] = K[Bx(i)];
      continue;
    }
    if (op === 4) {
      // LOADKX + EXTRAARG
      const ax = code[pc++];
      R[a] = K[ax >>> 7];
      continue;
    }
    if (op === 81) {
      R[a] = false;
      continue;
    }
    if (op === 83) {
      R[a] = true;
      continue;
    }
    if (op === 6) {
      // LOADNIL
      for (let j = 0; j <= B(i); j++) R[a + j] = null;
      continue;
    }
    if (op === 0) {
      // MOVE
      R[a] = R[B(i)];
      continue;
    }
    if (op === 17) {
      // NEWTABLE54 — Playdate always emits a following EXTRAARG
      pc++; // skip EXTRAARG
      R[a] = new LuaTable();
      continue;
    }
    if (op === 16) {
      // SETFIELD A B Ck : R[A][K[B]] := RK(C)
      const key = K[B(i)];
      const val = kBit(i) ? K[C(i)] : R[C(i)];
      R[a].set(key, val);
      continue;
    }
    if (op === 14) {
      // SETTABLE54
      const key = R[B(i)];
      const val = kBit(i) ? K[C(i)] : R[C(i)];
      R[a].set(key, val);
      continue;
    }
    if (op === 15) {
      // SETI
      const key = B(i);
      const val = kBit(i) ? K[C(i)] : R[C(i)];
      R[a].set(key, val);
      continue;
    }
    if (op === 76) {
      // SETLIST54
      let nstore = B(i);
      let start = C(i);
      if (kBit(i)) {
        const ax = code[pc++];
        start += (ax >>> 7) * 256;
      }
      if (nstore === 0) throw new Error("LuaT: SETLIST B=0 unsupported");
      const tab = R[a];
      for (let j = 1; j <= nstore; j++) tab.set(start + j, R[a + j]);
      continue;
    }
    if (op === 68 || op === 70) {
      // RETURN54 / RETURN1
      return toJs(R[a]);
    }
    if (op === 69) return null;
    if (op === 80) throw new Error(`LuaT: stray EXTRAARG at pc ${pc - 1}`);
    throw new Error(`LuaT: unsupported opcode ${op} at pc ${pc - 1}`);
  }
  throw new Error("LuaT: fell off end without RETURN");
}

/**
 * @param {Uint8Array} luac
 * @returns {object} plain Tiled-like map root
 */
export function worldmapObjectFromLuac(luac) {
  const fn = parseLuac(luac);
  if (fn.funcs && fn.funcs.length) {
    throw new Error("LuaT: unexpected nested functions in worldmap");
  }
  return evaluateTableChunk(fn);
}

/**
 * @param {Uint8Array} luac
 * @returns {Uint8Array} ZMAP bytes
 */
export function worldmapBinFromLuac(luac) {
  return encodeZmap(worldmapObjectFromLuac(luac));
}
