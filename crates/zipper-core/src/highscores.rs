//! Local high-score board — browser stand-in for Panic Catalog `scoreboards`.
//!
//! 1.10 submits remaining blood via `playdate.scoreboards.addScore("highscores", score)`
//! then draws up to 5 Catalog rows. The WASM host cannot reach Catalog, so we keep a
//! sorted list in `localStorage` (`zipper.hs.v1`) with a fixed player name `"you"`.

use crate::pft::Font;

/// Keep more than we draw so a later higher score can displace older mid-board entries.
pub const HS_STORE_MAX: usize = 10;
/// Lua `math.min(numScores, 5)` in `scoresLoaded`.
pub const HS_DRAW_MAX: usize = 5;
const HS_PERIOD_WIDTH: i32 = 4;
/// Target advance width for a padded score line (`highscoretable.lua` budget).
/// Draw uses this as the right edge for the numeric suffix so `181` / `245` align.
pub const HS_DOT_PAD: i32 = 148;
/// Offline stand-in for Catalog usernames.
pub const HS_PLAYER_NAME: &str = "you";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighScoreEntry {
    pub player: String,
    pub value: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HighScoreBoard {
    pub scores: Vec<HighScoreEntry>,
}

impl HighScoreBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `value` as `"you"`, sort high→low, trim to [`HS_STORE_MAX`].
    pub fn submit(&mut self, value: i32) {
        self.scores.push(HighScoreEntry {
            player: HS_PLAYER_NAME.to_string(),
            value,
        });
        self.scores.sort_by(|a, b| b.value.cmp(&a.value));
        if self.scores.len() > HS_STORE_MAX {
            self.scores.truncate(HS_STORE_MAX);
        }
    }

    /// Period-padded rows for the panel (`name…..value\n`), top [`HS_DRAW_MAX`].
    pub fn format_table_text(&self, font: Option<&Font>) -> String {
        let mut out = String::new();
        for (i, e) in self.scores.iter().take(HS_DRAW_MAX).enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&format_padded_line(font, &e.player, e.value));
        }
        out
    }

    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(64 + self.scores.len() * 32);
        out.push_str("{\"v\":1,\"scores\":[");
        for (i, e) in self.scores.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str("{\"player\":\"");
            push_json_string_escaped(&mut out, &e.player);
            out.push_str("\",\"value\":");
            out.push_str(&e.value.to_string());
            out.push('}');
        }
        out.push_str("]}");
        out
    }

    /// Parse host JSON. Unknown shape → empty board (not `None`) so boot stays resilient.
    pub fn from_json(s: &str) -> Self {
        let Some(v) = parse_value(s.trim()) else {
            return Self::default();
        };
        let Some(obj) = v.as_object() else {
            return Self::default();
        };
        let Some(arr) = obj.get("scores").and_then(JsonValue::as_array) else {
            return Self::default();
        };
        let mut scores = Vec::new();
        for item in arr {
            let Some(o) = item.as_object() else {
                continue;
            };
            let player = o
                .get("player")
                .and_then(JsonValue::as_str)
                .unwrap_or(HS_PLAYER_NAME)
                .to_string();
            let Some(value) = o.get("value").and_then(JsonValue::as_i32) else {
                continue;
            };
            scores.push(HighScoreEntry { player, value });
        }
        scores.sort_by(|a, b| b.value.cmp(&a.value));
        if scores.len() > HS_STORE_MAX {
            scores.truncate(HS_STORE_MAX);
        }
        Self { scores }
    }
}

/// Shared pad math with `format_last_score_line` (`highscoretable.lua`).
///
/// Lua floors `(148 - label - value) / periodWidth`. Measuring label and value
/// separately under-counts the junction tracking when they are concatenated, so a
/// thinner value (e.g. `181` vs `245`) can gain an extra period and overshoot by
/// ~2px. We keep the Lua floor as a starting guess, then shrink until the drawn
/// advance is ≤ [`HS_DOT_PAD`]. Numbers are right-aligned at draw time to that
/// same column so sibling rows share a right edge.
pub fn format_padded_line(font: Option<&Font>, label: &str, value: i32) -> String {
    let score_s = value.to_string();
    let (start_w, num_w) = if let Some(f) = font {
        (f.measure_text(label, 1), f.measure_text(&score_s, 1))
    } else {
        ((label.len() as i32) * 6, (score_s.len() as i32) * 6)
    };
    let mut num_periods = ((HS_DOT_PAD - start_w - num_w) / HS_PERIOD_WIDTH).max(0);
    if let Some(f) = font {
        while num_periods > 0 {
            let candidate = padded_string(label, num_periods, &score_s);
            if f.measure_text(&candidate, 1) <= HS_DOT_PAD {
                break;
            }
            num_periods -= 1;
        }
    }
    padded_string(label, num_periods, &score_s)
}

fn padded_string(label: &str, num_periods: i32, score_s: &str) -> String {
    let mut out = String::with_capacity(label.len() + num_periods as usize + score_s.len());
    out.push_str(label);
    for _ in 0..num_periods {
        out.push('.');
    }
    out.push_str(score_s);
    out
}

/// Split `label…digits` into prefix (label + periods) and trailing number.
pub fn split_score_line(text: &str) -> (&str, &str) {
    let bytes = text.as_bytes();
    let mut i = bytes.len();
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == 0 || i == bytes.len() {
        return (text, "");
    }
    (&text[..i], &text[i..])
}

fn push_json_string_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
}

// --- Minimal JSON (same subset as `save.rs`; kept local to avoid coupling) ---

#[derive(Debug, Clone)]
enum JsonValue {
    Null,
    Bool,
    Number(f64),
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
    fn as_i32(&self) -> Option<i32> {
        match self {
            Self::Number(n) if n.is_finite() && *n >= i32::MIN as f64 && *n <= i32::MAX as f64 => {
                Some(*n as i32)
            }
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
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
    let c = *b.get(*i)?;
    match c {
        b'n' => {
            if b.get(*i..*i + 4)? == b"null" {
                *i += 4;
                Some(JsonValue::Null)
            } else {
                None
            }
        }
        b't' => {
            if b.get(*i..*i + 4)? == b"true" {
                *i += 4;
                Some(JsonValue::Bool)
            } else {
                None
            }
        }
        b'f' => {
            if b.get(*i..*i + 5)? == b"false" {
                *i += 5;
                Some(JsonValue::Bool)
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
    if *b.get(*i)? != b'"' {
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
                    b'u' => {
                        let hex = b.get(*i..*i + 4)?;
                        *i += 4;
                        let s = std::str::from_utf8(hex).ok()?;
                        let cp = u32::from_str_radix(s, 16).ok()?;
                        out.push(char::from_u32(cp)?);
                    }
                    _ => return None,
                }
            }
            c => out.push(c as char),
        }
    }
    None
}

fn parse_array(b: &[u8], i: &mut usize) -> Option<JsonValue> {
    *i += 1;
    let mut items = Vec::new();
    skip_ws(b, i);
    if *b.get(*i)? == b']' {
        *i += 1;
        return Some(JsonValue::Array(items));
    }
    loop {
        items.push(parse_value_at(b, i)?);
        skip_ws(b, i);
        match *b.get(*i)? {
            b']' => {
                *i += 1;
                return Some(JsonValue::Array(items));
            }
            b',' => *i += 1,
            _ => return None,
        }
    }
}

fn parse_object(b: &[u8], i: &mut usize) -> Option<JsonValue> {
    *i += 1;
    let mut map = std::collections::BTreeMap::new();
    skip_ws(b, i);
    if *b.get(*i)? == b'}' {
        *i += 1;
        return Some(JsonValue::Object(map));
    }
    loop {
        skip_ws(b, i);
        let key = parse_string(b, i)?;
        skip_ws(b, i);
        if *b.get(*i)? != b':' {
            return None;
        }
        *i += 1;
        let val = parse_value_at(b, i)?;
        map.insert(key, val);
        skip_ws(b, i);
        match *b.get(*i)? {
            b'}' => {
                *i += 1;
                return Some(JsonValue::Object(map));
            }
            b',' => *i += 1,
            _ => return None,
        }
    }
}

fn parse_number(b: &[u8], i: &mut usize) -> Option<JsonValue> {
    let start = *i;
    if b[*i] == b'-' {
        *i += 1;
    }
    while *i < b.len() && b[*i].is_ascii_digit() {
        *i += 1;
    }
    if *i < b.len() && b[*i] == b'.' {
        *i += 1;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
    }
    if *i < b.len() && matches!(b[*i], b'e' | b'E') {
        *i += 1;
        if *i < b.len() && matches!(b[*i], b'+' | b'-') {
            *i += 1;
        }
        while *i < b.len() && b[*i].is_ascii_digit() {
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
    fn submit_sorts_and_caps() {
        let mut b = HighScoreBoard::new();
        for v in [10, 50, 30, 40, 20, 60, 5, 70, 15, 25, 80] {
            b.submit(v);
        }
        assert_eq!(b.scores.len(), HS_STORE_MAX);
        assert_eq!(b.scores[0].value, 80);
        assert_eq!(b.scores[1].value, 70);
        // 11 submits → drop lowest (5); bottom of the capped board is 10.
        assert_eq!(b.scores.last().unwrap().value, 10);
        assert!(b.scores.iter().all(|e| e.player == HS_PLAYER_NAME));
    }

    #[test]
    fn json_round_trip() {
        let mut b = HighScoreBoard::new();
        b.submit(180);
        b.submit(90);
        let json = b.to_json();
        let back = HighScoreBoard::from_json(&json);
        assert_eq!(back.scores.len(), 2);
        assert_eq!(back.scores[0].value, 180);
        assert_eq!(back.scores[1].value, 90);
    }

    #[test]
    fn format_table_top_five() {
        let mut b = HighScoreBoard::new();
        for v in [1, 2, 3, 4, 5, 6] {
            b.submit(v);
        }
        let text = b.format_table_text(None);
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), HS_DRAW_MAX);
        assert!(lines[0].starts_with("you") && lines[0].ends_with('6'));
        assert!(lines[4].ends_with('2'));
    }

    #[test]
    fn padded_lines_stay_within_budget_when_font_present() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../www/assets/demo/monoblack.pft"
        );
        let Ok(data) = std::fs::read(path) else {
            return;
        };
        let Ok(font) = crate::pft::decode_pft(&data) else {
            return;
        };
        for value in [181, 245, 99, 200] {
            let line = format_padded_line(Some(&font), HS_PLAYER_NAME, value);
            let w = font.measure_text(&line, 1);
            assert!(
                w <= HS_DOT_PAD,
                "value {value}: width {w} exceeds budget {HS_DOT_PAD} ({line:?})"
            );
            let (prefix, number) = split_score_line(&line);
            assert_eq!(number, value.to_string());
            // Right-aligned number ends on the shared column.
            let num_x = HS_DOT_PAD - font.measure_text(number, 1);
            assert_eq!(
                num_x + font.measure_text(number, 1),
                HS_DOT_PAD,
                "value {value} number right edge"
            );
            assert!(
                font.measure_text(prefix, 1) <= num_x,
                "value {value}: prefix overlaps number"
            );
        }
    }
}
