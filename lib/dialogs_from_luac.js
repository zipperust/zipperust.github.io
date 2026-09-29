/**
 * Extract `dialogs` from Zipper `script.luac` (Playdate LuaT).
 *
 * The main chunk assigns globals via SETTABUP and builds nested tables;
 * nested CLOSUREs (saveProgress / etc.) are stored but not executed.
 *
 * Reuses the LuaT parser from `./worldmap_from_luac.js`.
 */

import { parseLuac } from "./worldmap_from_luac.js";

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
  /** Dense 1-based array → JS array (holes become null). */
  toArray() {
    if (this.array.size === 0) return [];
    const m = Math.max(...this.array.keys());
    const arr = new Array(m);
    for (let i = 1; i <= m; i++) {
      const v = this.array.get(i);
      arr[i - 1] = v === undefined ? null : tableToPlain(v);
    }
    return arr;
  }
}

function tableToPlain(v) {
  if (v instanceof LuaTable) return v.toArray();
  return v;
}

/** Opaque function proto placeholder (never called). */
class Closure {
  constructor(proto) {
    this.proto = proto;
  }
}

const OP = (i) => i & 0x7f;
const A = (i) => (i >>> 7) & 0xff;
const B = (i) => (i >>> 16) & 0xff;
const C = (i) => (i >>> 24) & 0xff;
const kBit = (i) => (i >>> 15) & 1;
const Bx = (i) => (i >>> 15) & 0x1ffff;
const sBx = (i) => Bx(i) - 0xffff;

/**
 * Evaluate a chunk that only builds `_ENV` globals (no function calls).
 * @param {{code: number[], constants: any[], maxstack: number, funcs: any[]}} fn
 * @returns {LuaTable} _ENV
 */
export function evaluateEnvChunk(fn) {
  const code = fn.code;
  const K = fn.constants;
  const R = new Array(fn.maxstack + 16).fill(undefined);
  const env = new LuaTable();
  const ups = [env]; // upvalue 0 = _ENV
  let pc = 0;
  const n = code.length;

  while (pc < n) {
    const i = code[pc++];
    const op = OP(i);
    const a = A(i);

    if (op === 79) continue; // VARARGPREP

    if (op === 1) {
      R[a] = sBx(i);
      continue;
    }
    if (op === 2) {
      R[a] = sBx(i);
      continue;
    }
    if (op === 3) {
      R[a] = K[Bx(i)];
      continue;
    }
    if (op === 4) {
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
      for (let j = 0; j <= B(i); j++) R[a + j] = null;
      continue;
    }
    if (op === 0) {
      R[a] = R[B(i)];
      continue;
    }
    if (op === 17) {
      pc++; // EXTRAARG
      R[a] = new LuaTable();
      continue;
    }
    if (op === 16) {
      const key = K[B(i)];
      const val = kBit(i) ? K[C(i)] : R[C(i)];
      R[a].set(key, val);
      continue;
    }
    if (op === 14) {
      const key = R[B(i)];
      const val = kBit(i) ? K[C(i)] : R[C(i)];
      R[a].set(key, val);
      continue;
    }
    if (op === 15) {
      const key = B(i);
      const val = kBit(i) ? K[C(i)] : R[C(i)];
      R[a].set(key, val);
      continue;
    }
    if (op === 13) {
      // SETTABUP A B Ck : up[A][K[B]] := RK(C)
      const tab = ups[a];
      const key = K[B(i)];
      const val = kBit(i) ? K[C(i)] : R[C(i)];
      tab.set(key, val);
      continue;
    }
    if (op === 76) {
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
    if (op === 77) {
      // CLOSURE A Bx — store proto; do not run
      R[a] = new Closure(fn.funcs[Bx(i)]);
      continue;
    }
    if (op === 68 || op === 70 || op === 69) {
      return env;
    }
    if (op === 80) throw new Error(`LuaT: stray EXTRAARG at pc ${pc - 1}`);
    throw new Error(`LuaT: unsupported opcode ${op} at pc ${pc - 1}`);
  }
  return env;
}

/**
 * @param {Uint8Array} luac `script.luac` bytes
 * @returns {string[][]} 31 scripts, each a list of lines (incl. loop/hold/stop)
 */
export function dialogsFromScriptLuac(luac) {
  const fn = parseLuac(luac);
  const env = evaluateEnvChunk(fn);
  const dialogs = env.get("dialogs");
  if (!(dialogs instanceof LuaTable)) {
    throw new Error('LuaT: script.luac missing global "dialogs" table');
  }
  const scripts = dialogs.toArray();
  if (!Array.isArray(scripts) || scripts.length === 0) {
    throw new Error("LuaT: dialogs table empty");
  }
  return scripts.map((script, i) => {
    if (!Array.isArray(script)) {
      throw new Error(`LuaT: dialogs[${i + 1}] is not an array`);
    }
    return script.map((line) => (line == null ? "" : String(line)));
  });
}
