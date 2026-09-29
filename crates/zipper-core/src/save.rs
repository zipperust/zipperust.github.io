//! Mid-run `"save"` datastore (`utils.lua` writesave / deletesave / check_for_save /
//! `gameWillTerminate`). Host persists JSON under `localStorage` key `zipper.save.v1`.
//!
//! Dialog counters (`dialog_progress_save`) are **not** part of this blob.

/// Gate for `check_for_save` version reject (`utils.lua`). Bump when the blob shape
/// becomes incompatible; missing / lower version → treat as no resume.
pub const SAVE_VERSION: u32 = 1;

/// One parked corpse restored into `globalenemies` (`saved_game.deadenemies`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadEnemySave {
    pub x: i32,
    pub y: i32,
    /// Lua `enemy.etype` (`kSwordsman`…); Spirit corpses are skipped on write/restore.
    pub etype: u8,
    /// Lua facing enum 1..=4.
    pub facing: u8,
}

/// In-memory / JSON stand-in for Playdate `playdate.datastore` `"save"`.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedGame {
    /// `1` → next boot resumes via `check_for_save`; `0` → ignore blob for resume.
    pub loadsave: u8,
    pub version: u32,
    pub seed: u32,
    pub blood: i32,
    pub key: bool,
    /// `[tile_x, tile_y, cam_x, cam_y, facing]` — Lua `playerPos`.
    pub player_pos: Option<[i32; 5]>,
    /// Length 7; index 0 unused; swordsman (1) starts seen in a fresh run.
    pub enemies_seen: Vec<bool>,
    pub enemies_killed: Vec<bool>,
    /// Parallel to world NPC list order (`kind == 1`).
    pub npcs_visited: Vec<bool>,
    pub dead_enemies: Vec<DeadEnemySave>,
}

impl Default for SavedGame {
    fn default() -> Self {
        Self {
            loadsave: 0,
            version: SAVE_VERSION,
            seed: 0,
            blood: 0,
            key: false,
            player_pos: None,
            enemies_seen: vec![false; 7],
            enemies_killed: vec![false; 7],
            npcs_visited: Vec::new(),
            dead_enemies: Vec::new(),
        }
    }
}

impl SavedGame {
    /// True when this blob should drive boot seed + `check_for_save`.
    pub fn should_resume(&self) -> bool {
        self.loadsave == 1 && self.version_ok()
    }

    /// Lua: missing version or `version < game_version and game_version < 1` → wipe.
    /// Port game version is always ≥ 1 conceptually; reject only missing / foreign.
    pub fn version_ok(&self) -> bool {
        self.version > 0 && self.version <= SAVE_VERSION
    }

    /// Serialize to a compact JSON object (host `localStorage`).
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(256);
        out.push('{');
        push_u32_field(&mut out, "loadsave", self.loadsave as u32, true);
        push_u32_field(&mut out, "version", self.version, false);
        push_u32_field(&mut out, "seed", self.seed, false);
        push_i32_field(&mut out, "blood", self.blood, false);
        push_bool_field(&mut out, "key", self.key, false);
        if let Some(pos) = self.player_pos {
            out.push_str(",\"playerPos\":[");
            for (i, v) in pos.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&v.to_string());
            }
            out.push(']');
        }
        push_bool_array(&mut out, "enemiesSeen", &self.enemies_seen);
        push_bool_array(&mut out, "enemiesKilled", &self.enemies_killed);
        push_bool_array(&mut out, "npcs_visited", &self.npcs_visited);
        out.push_str(",\"deadenemies\":[");
        for (i, d) in self.dead_enemies.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('{');
            push_i32_field(&mut out, "x", d.x, true);
            push_i32_field(&mut out, "y", d.y, false);
            push_u32_field(&mut out, "etype", d.etype as u32, false);
            push_u32_field(&mut out, "facing", d.facing as u32, false);
            out.push('}');
        }
        out.push_str("]}");
        out
    }

    /// Parse host JSON. Unknown / truncated fields get defaults; returns `None` on
    /// hard parse failure (not an object).
    pub fn from_json(s: &str) -> Option<Self> {
        let v = parse_value(s.trim())?;
        let obj = v.as_object()?;
        let mut g = SavedGame::default();
        if let Some(n) = obj.get("loadsave").and_then(JsonValue::as_u32) {
            g.loadsave = n.min(255) as u8;
        }
        if let Some(n) = obj.get("version").and_then(JsonValue::as_u32) {
            g.version = n;
        } else {
            // Lua rejects missing version.
            g.version = 0;
        }
        if let Some(n) = obj.get("seed").and_then(JsonValue::as_u32) {
            g.seed = n;
        }
        if let Some(n) = obj.get("blood").and_then(JsonValue::as_i32) {
            g.blood = n;
        }
        if let Some(b) = obj.get("key").and_then(JsonValue::as_bool) {
            g.key = b;
        }
        if let Some(arr) = obj.get("playerPos").and_then(JsonValue::as_array) {
            if arr.len() >= 5 {
                let mut pos = [0i32; 5];
                let mut ok = true;
                for i in 0..5 {
                    if let Some(n) = arr[i].as_i32() {
                        pos[i] = n;
                    } else {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    g.player_pos = Some(pos);
                }
            }
        }
        if let Some(arr) = obj.get("enemiesSeen").and_then(JsonValue::as_array) {
            g.enemies_seen = bools_from_json_array(arr, 7);
        }
        if let Some(arr) = obj.get("enemiesKilled").and_then(JsonValue::as_array) {
            g.enemies_killed = bools_from_json_array(arr, 7);
        }
        if let Some(arr) = obj.get("npcs_visited").and_then(JsonValue::as_array) {
            g.npcs_visited = arr.iter().map(|v| v.as_bool().unwrap_or(false)).collect();
        }
        if let Some(arr) = obj.get("deadenemies").and_then(JsonValue::as_array) {
            for item in arr {
                let Some(o) = item.as_object() else {
                    continue;
                };
                let Some(x) = o.get("x").and_then(JsonValue::as_i32) else {
                    continue;
                };
                let Some(y) = o.get("y").and_then(JsonValue::as_i32) else {
                    continue;
                };
                let etype = o.get("etype").and_then(JsonValue::as_u32).unwrap_or(0) as u8;
                let facing = o.get("facing").and_then(JsonValue::as_u32).unwrap_or(2) as u8;
                g.dead_enemies.push(DeadEnemySave {
                    x,
                    y,
                    etype,
                    facing,
                });
            }
        }
        Some(g)
    }
}

fn bools_from_json_array(arr: &[JsonValue], min_len: usize) -> Vec<bool> {
    let mut out: Vec<bool> = arr.iter().map(|v| v.as_bool().unwrap_or(false)).collect();
    if out.len() < min_len {
        out.resize(min_len, false);
    }
    out
}

fn push_u32_field(out: &mut String, key: &str, val: u32, first: bool) {
    if !first {
        out.push(',');
    }
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    out.push_str(&val.to_string());
}

fn push_i32_field(out: &mut String, key: &str, val: i32, first: bool) {
    if !first {
        out.push(',');
    }
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    out.push_str(&val.to_string());
}

fn push_bool_field(out: &mut String, key: &str, val: bool, first: bool) {
    if !first {
        out.push(',');
    }
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    out.push_str(if val { "true" } else { "false" });
}

fn push_bool_array(out: &mut String, key: &str, vals: &[bool]) {
    out.push(',');
    out.push('"');
    out.push_str(key);
    out.push_str("\":[");
    for (i, b) in vals.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(if *b { "true" } else { "false" });
    }
    out.push(']');
}

// --- Minimal JSON subset (object / array / number / bool / null / string) ---

#[derive(Debug, Clone)]
enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    #[allow(dead_code)] // Parsed for completeness; save blobs do not use string values.
    String(String),
    Array(Vec<JsonValue>),
    Object(std::collections::BTreeMap<String, JsonValue>),
}

impl JsonValue {
    fn as_object(&self) -> Option<&std::collections::BTreeMap<String, JsonValue>> {
        match self {
            Self::Object(m) => Some(m),
            _ => None,
        }
    }
    fn as_array(&self) -> Option<&[JsonValue]> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }
    fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }
    fn as_u32(&self) -> Option<u32> {
        match self {
            Self::Number(n) if n.is_finite() && *n >= 0.0 && *n <= u32::MAX as f64 => {
                Some(*n as u32)
            }
            _ => None,
        }
    }
    fn as_i32(&self) -> Option<i32> {
        match self {
            Self::Number(n) if n.is_finite() && *n >= i32::MIN as f64 && *n <= i32::MAX as f64 => {
                Some(*n as i32)
            }
            _ => None,
        }
    }
}

fn parse_value(s: &str) -> Option<JsonValue> {
    let mut i = 0;
    let v = parse_value_at(s.as_bytes(), &mut i)?;
    skip_ws(s.as_bytes(), &mut i);
    if i != s.len() {
        return None;
    }
    Some(v)
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && matches!(b[*i], b' ' | b'\n' | b'\r' | b'\t') {
        *i += 1;
    }
}

fn parse_value_at(b: &[u8], i: &mut usize) -> Option<JsonValue> {
    skip_ws(b, i);
    if *i >= b.len() {
        return None;
    }
    match b[*i] {
        b'n' => {
            if b.get(*i..*i + 4) == Some(b"null") {
                *i += 4;
                Some(JsonValue::Null)
            } else {
                None
            }
        }
        b't' => {
            if b.get(*i..*i + 4) == Some(b"true") {
                *i += 4;
                Some(JsonValue::Bool(true))
            } else {
                None
            }
        }
        b'f' => {
            if b.get(*i..*i + 5) == Some(b"false") {
                *i += 5;
                Some(JsonValue::Bool(false))
            } else {
                None
            }
        }
        b'"' => parse_string(b, i).map(JsonValue::String),
        b'[' => parse_array(b, i),
        b'{' => parse_object(b, i),
        b'-' | b'0'..=b'9' => parse_number(b, i),
        _ => None,
    }
}

fn parse_string(b: &[u8], i: &mut usize) -> Option<String> {
    if b.get(*i) != Some(&b'"') {
        return None;
    }
    *i += 1;
    let mut out = String::new();
    while *i < b.len() {
        let c = b[*i];
        *i += 1;
        match c {
            b'"' => return Some(out),
            b'\\' => {
                let e = *b.get(*i)?;
                *i += 1;
                match e {
                    b'"' | b'\\' | b'/' => out.push(e as char),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    _ => return None,
                }
            }
            _ => out.push(c as char),
        }
    }
    None
}

fn parse_array(b: &[u8], i: &mut usize) -> Option<JsonValue> {
    if b.get(*i) != Some(&b'[') {
        return None;
    }
    *i += 1;
    let mut items = Vec::new();
    skip_ws(b, i);
    if b.get(*i) == Some(&b']') {
        *i += 1;
        return Some(JsonValue::Array(items));
    }
    loop {
        items.push(parse_value_at(b, i)?);
        skip_ws(b, i);
        match b.get(*i)? {
            b']' => {
                *i += 1;
                return Some(JsonValue::Array(items));
            }
            b',' => {
                *i += 1;
            }
            _ => return None,
        }
    }
}

fn parse_object(b: &[u8], i: &mut usize) -> Option<JsonValue> {
    if b.get(*i) != Some(&b'{') {
        return None;
    }
    *i += 1;
    let mut map = std::collections::BTreeMap::new();
    skip_ws(b, i);
    if b.get(*i) == Some(&b'}') {
        *i += 1;
        return Some(JsonValue::Object(map));
    }
    loop {
        skip_ws(b, i);
        let key = parse_string(b, i)?;
        skip_ws(b, i);
        if b.get(*i)? != &b':' {
            return None;
        }
        *i += 1;
        let val = parse_value_at(b, i)?;
        map.insert(key, val);
        skip_ws(b, i);
        match b.get(*i)? {
            b'}' => {
                *i += 1;
                return Some(JsonValue::Object(map));
            }
            b',' => {
                *i += 1;
            }
            _ => return None,
        }
    }
}

fn parse_number(b: &[u8], i: &mut usize) -> Option<JsonValue> {
    let start = *i;
    if b.get(*i) == Some(&b'-') {
        *i += 1;
    }
    if *i >= b.len() {
        return None;
    }
    if b[*i] == b'0' {
        *i += 1;
    } else if matches!(b[*i], b'1'..=b'9') {
        while *i < b.len() && matches!(b[*i], b'0'..=b'9') {
            *i += 1;
        }
    } else {
        return None;
    }
    if b.get(*i) == Some(&b'.') {
        *i += 1;
        if *i >= b.len() || !matches!(b[*i], b'0'..=b'9') {
            return None;
        }
        while *i < b.len() && matches!(b[*i], b'0'..=b'9') {
            *i += 1;
        }
    }
    let s = std::str::from_utf8(&b[start..*i]).ok()?;
    let n: f64 = s.parse().ok()?;
    Some(JsonValue::Number(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_game_json_round_trip() {
        let mut g = SavedGame::default();
        g.loadsave = 1;
        g.version = SAVE_VERSION;
        g.seed = 42;
        g.blood = 200;
        g.key = true;
        g.player_pos = Some([110, 180, 108, 162, 1]);
        g.enemies_seen = vec![false, true, true, false, false, false, false];
        g.enemies_killed = vec![false, true, false, false, false, false, false];
        g.npcs_visited = vec![true, false, true];
        g.dead_enemies.push(DeadEnemySave {
            x: 100,
            y: 170,
            etype: 1,
            facing: 2,
        });
        let json = g.to_json();
        let back = SavedGame::from_json(&json).expect("parse");
        assert_eq!(back.loadsave, 1);
        assert_eq!(back.seed, 42);
        assert_eq!(back.blood, 200);
        assert!(back.key);
        assert_eq!(back.player_pos, Some([110, 180, 108, 162, 1]));
        assert_eq!(back.enemies_seen[1], true);
        assert_eq!(back.enemies_seen[2], true);
        assert_eq!(back.enemies_killed[1], true);
        assert_eq!(back.npcs_visited, vec![true, false, true]);
        assert_eq!(back.dead_enemies.len(), 1);
        assert_eq!(back.dead_enemies[0].x, 100);
        assert_eq!(back.dead_enemies[0].etype, 1);
        assert!(back.should_resume());
    }

    #[test]
    fn version_zero_rejects_resume() {
        let mut g = SavedGame::default();
        g.loadsave = 1;
        g.version = 0;
        assert!(!g.should_resume());
    }

    #[test]
    fn loadsave_zero_no_resume() {
        let mut g = SavedGame::default();
        g.loadsave = 0;
        g.version = SAVE_VERSION;
        assert!(!g.should_resume());
    }
}
