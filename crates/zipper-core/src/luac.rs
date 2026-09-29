//! Minimal Playdate LuaT (`\x1bLuaT`) helpers for BYOA extracts.
//!
//! Enough to pull authored tables (e.g. `introchord`) from `Globals.luac`
//! without a full Lua VM. Shared by wasm and future non-JS hosts.

use std::path::PathBuf;

/// Playdate Lua 5.4 / LuaT parse or extract failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LuacError {
    BadSignature,
    BadVersion,
    BadHeader,
    Truncated(&'static str),
    UnsupportedSizes,
    BadConstant(u8),
    IntrochordNotFound,
    IntrochordEmpty,
    CreditsNotFound,
}

impl std::fmt::Display for LuacError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadSignature => write!(f, "LuaT: bad signature"),
            Self::BadVersion => write!(f, "LuaT: expected Lua 5.4"),
            Self::BadHeader => write!(f, "LuaT: bad header"),
            Self::Truncated(what) => write!(f, "LuaT: truncated ({what})"),
            Self::UnsupportedSizes => write!(f, "LuaT: unsupported instr/int/num sizes"),
            Self::BadConstant(t) => write!(f, "LuaT: unknown constant type {t}"),
            Self::IntrochordNotFound => write!(f, "LuaT: introchord assignment not found"),
            Self::IntrochordEmpty => write!(f, "LuaT: introchord table empty"),
            Self::CreditsNotFound => write!(f, "LuaT: ending credits not found"),
        }
    }
}

impl std::error::Error for LuacError {}

#[derive(Debug)]
struct Proto {
    code: Vec<u32>,
    constants: Vec<Constant>,
    funcs: Vec<Proto>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Bool/Int/Float kept for a complete LuaT constant map.
enum Constant {
    Nil,
    Bool(bool),
    Int(i32),
    Float(f32),
    Str(String),
}

struct Reader<'a> {
    data: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, i: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.i)
    }

    fn u8(&mut self) -> Result<u8, LuacError> {
        if self.i >= self.data.len() {
            return Err(LuacError::Truncated("u8"));
        }
        let v = self.data[self.i];
        self.i += 1;
        Ok(v)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], LuacError> {
        if self.remaining() < n {
            return Err(LuacError::Truncated("bytes"));
        }
        let s = &self.data[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }

    fn i32(&mut self) -> Result<i32, LuacError> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u32(&mut self) -> Result<u32, LuacError> {
        Ok(self.i32()? as u32)
    }

    fn f32(&mut self) -> Result<f32, LuacError> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// BIntegerType54 — 7-bit big-endian chunks, stop when high bit set.
    fn varint(&mut self) -> Result<usize, LuacError> {
        let mut x: usize = 0;
        loop {
            let b = self.u8()?;
            x = (x << 7) | (b as usize & 0x7f);
            if b & 0x80 != 0 {
                return Ok(x);
            }
        }
    }

    fn string(&mut self) -> Result<String, LuacError> {
        let size = self.varint()?;
        if size == 0 {
            return Ok(String::new());
        }
        // LStringType54: size includes trailing NUL; payload is size-1 bytes.
        let payload = self.take(size - 1)?;
        Ok(String::from_utf8_lossy(payload).into_owned())
    }
}

fn parse_constant(r: &mut Reader<'_>) -> Result<Constant, LuacError> {
    let t = r.u8()?;
    Ok(match t {
        0 => Constant::Nil,
        1 => Constant::Bool(false),
        17 => Constant::Bool(true),
        3 => Constant::Int(r.i32()?),
        19 => Constant::Float(r.f32()?),
        4 | 20 => Constant::Str(r.string()?),
        other => return Err(LuacError::BadConstant(other)),
    })
}

fn parse_function(r: &mut Reader<'_>) -> Result<Proto, LuacError> {
    let _source = r.string()?;
    let _linedefined = r.varint()?;
    let _lastlinedefined = r.varint()?;
    let _nparams = r.u8()?;
    let _vararg = r.u8()?;
    let _maxstack = r.u8()?;
    let ncode = r.varint()?;
    let mut code = Vec::with_capacity(ncode);
    for _ in 0..ncode {
        code.push(r.u32()?);
    }
    let nconst = r.varint()?;
    let mut constants = Vec::with_capacity(nconst);
    for _ in 0..nconst {
        constants.push(parse_constant(r)?);
    }
    let nups = r.varint()?;
    for _ in 0..nups {
        let _ = r.u8()?;
        let _ = r.u8()?;
        let _ = r.u8()?;
    }
    let nfuncs = r.varint()?;
    let mut funcs = Vec::with_capacity(nfuncs);
    for _ in 0..nfuncs {
        // Nested protos (introchord lives in the main chunk; credits nest one level).
        funcs.push(parse_function(r)?);
    }
    // Debug info
    let nlines = r.varint()?;
    let _ = r.take(nlines)?;
    let nabs = r.varint()?;
    for _ in 0..nabs {
        let _ = r.varint()?;
        let _ = r.varint()?;
    }
    let nlocals = r.varint()?;
    for _ in 0..nlocals {
        let _ = r.string()?;
        let _ = r.varint()?;
        let _ = r.varint()?;
    }
    let nupnames = r.varint()?;
    for _ in 0..nupnames {
        let _ = r.string()?;
    }
    Ok(Proto {
        code,
        constants,
        funcs,
    })
}

/// Parse the main function proto from a Playdate LuaT chunk.
fn parse_luac_main(data: &[u8]) -> Result<Proto, LuacError> {
    let mut r = Reader::new(data);
    let sig = r.take(4)?;
    if sig != [0x1b, 0x4c, 0x75, 0x61] {
        return Err(LuacError::BadSignature);
    }
    if r.u8()? != 0x54 {
        return Err(LuacError::BadVersion);
    }
    if r.u8()? != 0 {
        return Err(LuacError::BadHeader);
    }
    let tail = r.take(6)?;
    if tail != [0x19, 0x93, 0x0d, 0x0a, 0x1a, 0x0a] {
        return Err(LuacError::BadHeader);
    }
    let instr = r.u8()?;
    let int_size = r.u8()?;
    let num_size = r.u8()?;
    if instr != 4 || int_size != 4 || num_size != 4 {
        return Err(LuacError::UnsupportedSizes);
    }
    if r.i32()? != 0x5678 {
        return Err(LuacError::BadHeader);
    }
    if (r.f32()? - 370.5).abs() > 1e-4 {
        return Err(LuacError::BadHeader);
    }
    let _main_ups = r.u8()?;
    parse_function(&mut r)
}

#[inline]
fn op(i: u32) -> u32 {
    i & 0x7f
}
#[inline]
fn bx(i: u32) -> u32 {
    (i >> 15) & 0x1ffff
}
#[inline]
fn sbx(i: u32) -> i32 {
    bx(i) as i32 - 0xffff
}
#[inline]
fn b_field(i: u32) -> u32 {
    (i >> 16) & 0xff
}

const OP_LOADI: u32 = 1;
const OP_LOADK: u32 = 3;
const OP_SETLIST: u32 = 76;

/// Extract `introchord` MIDI note numbers from `Globals.luac`.
///
/// Notes are LOADI immediates (not K-table numbers). Pattern after
/// `LOADK "introchord"`: skip to consecutive `LOADI`s, then `SETLIST`
/// with `B == count` (`Globals.lua` `introchord = {100,93,98,95,96,102}`).
pub fn introchord_from_globals_luac(data: &[u8]) -> Result<Vec<u8>, LuacError> {
    let proto = parse_luac_main(data)?;
    let mut loadk_pcs = Vec::new();
    for (pc, &ins) in proto.code.iter().enumerate() {
        if op(ins) != OP_LOADK {
            continue;
        }
        let idx = bx(ins) as usize;
        if let Some(Constant::Str(s)) = proto.constants.get(idx) {
            if s == "introchord" {
                loadk_pcs.push(pc);
            }
        }
    }
    for start in loadk_pcs {
        if let Some(notes) = try_chord_after_loadk(&proto.code, start) {
            if notes.is_empty() {
                return Err(LuacError::IntrochordEmpty);
            }
            return Ok(notes);
        }
    }
    Err(LuacError::IntrochordNotFound)
}

fn try_chord_after_loadk(code: &[u32], loadk_pc: usize) -> Option<Vec<u8>> {
    let mut pc = loadk_pc + 1;
    // Skip non-LOADI noise (SELF + EXTRAARG, NEWTABLE, …).
    while pc < code.len() && op(code[pc]) != OP_LOADI {
        pc += 1;
        // Bound: chord is immediate after the name load.
        if pc > loadk_pc + 16 {
            return None;
        }
    }
    let mut notes: Vec<u8> = Vec::new();
    while pc < code.len() && op(code[pc]) == OP_LOADI {
        let v = sbx(code[pc]);
        if !(0..=127).contains(&v) {
            return None;
        }
        notes.push(v as u8);
        pc += 1;
        if notes.len() > 32 {
            return None;
        }
    }
    if notes.is_empty() || pc >= code.len() {
        return None;
    }
    let setlist = code[pc];
    if op(setlist) != OP_SETLIST {
        return None;
    }
    let nstore = b_field(setlist);
    if nstore as usize != notes.len() {
        return None;
    }
    Some(notes)
}

/// Extract the ending-credit strings from `main.luac`.
///
/// `main.lua` shows authored credit cards with `creditscroller:show([[…]])`; the
/// prose is compiled in as multi-line string constants (only such strings in the
/// chunk). We return them in source order; the engine pairs them with the
/// (non-content) tick schedule in [`crate::ending::CREDIT_TICKS`].
pub fn credits_from_main_luac(data: &[u8]) -> Result<Vec<String>, LuacError> {
    let proto = parse_luac_main(data)?;
    let mut out = Vec::new();
    collect_multiline_strings(&proto, &mut out);
    if out.is_empty() {
        return Err(LuacError::CreditsNotFound);
    }
    Ok(out)
}

fn collect_multiline_strings(proto: &Proto, out: &mut Vec<String>) {
    for c in &proto.constants {
        if let Constant::Str(s) = c {
            if s.contains('\n') {
                out.push(s.clone());
            }
        }
    }
    for f in &proto.funcs {
        collect_multiline_strings(f, out);
    }
}

/// Path to staged demo/native `introchord.json` (offline extract).
pub fn introchord_json_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/introchord.json")
}

/// Parse a JSON array of MIDI note numbers (`[100,93,…]`).
pub fn introchord_from_json(text: &str) -> Result<Vec<u8>, String> {
    let t = text.trim();
    if !t.starts_with('[') || !t.ends_with(']') {
        return Err("introchord.json: expected array".into());
    }
    let inner = &t[1..t.len() - 1];
    let mut out = Vec::new();
    for part in inner.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let n: i32 = p
            .parse()
            .map_err(|_| format!("introchord.json: bad note {p:?}"))?;
        if !(0..=127).contains(&n) {
            return Err(format!("introchord.json: note out of range {n}"));
        }
        out.push(n as u8);
    }
    if out.is_empty() {
        return Err("introchord.json: empty".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn introchord_from_local_globals_luac() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../ref/extracted/main/Globals.luac");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skip: {path:?} missing");
            return;
        };
        let chord = introchord_from_globals_luac(&bytes).expect("extract");
        assert_eq!(chord, vec![100, 93, 98, 95, 96, 102]);
    }

    #[test]
    fn introchord_json_roundtrip_shape() {
        let notes = introchord_from_json("[100, 93, 98, 95, 96, 102]").unwrap();
        assert_eq!(notes, vec![100, 93, 98, 95, 96, 102]);
    }

    #[test]
    fn credits_from_local_main_luac() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../ref/extracted/main/main.luac");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skip: {path:?} missing");
            return;
        };
        let cards = credits_from_main_luac(&bytes).expect("extract");
        assert_eq!(cards.len(), 8, "ending shows 8 credit cards in 1.10");
    }
}
