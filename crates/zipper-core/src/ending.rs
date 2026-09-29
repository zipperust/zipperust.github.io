//! Win cinema, credits, and score screen (`main.lua` / `winbackground` / `credits` / `highscoretable`).
//!
//! Panic Catalog is unreachable from the browser; [`crate::highscores`] is the local stand-in
//! (`zipper.hs.v1`). The panel still draws Catalog-style top rows + `last score`.

use crate::bitmap::{Bitmap, DrawMode, ImageTable};
use crate::framebuffer::{Framebuffer, SCREEN_HEIGHT, SCREEN_WIDTH};
use crate::highscores::{format_padded_line, split_score_line, HS_DOT_PAD};
use crate::pft::Font;

/// `winbackground.lua` wave LUT (1-based in Lua; 0-based here).
pub const WIN_BG_LUT: &[f32] = &[ // 127 entries
    10_f32,
    10.499792_f32,
    10.998334_f32,
    11.494381_f32,
    11.986693_f32,
    12.47404_f32,
    12.955202_f32,
    13.428978_f32,
    13.894183_f32,
    14.349655_f32,
    14.794255_f32,
    15.226872_f32,
    15.646424_f32,
    16.051865_f32,
    16.442177_f32,
    16.816387_f32,
    17.173561_f32,
    17.512804_f32,
    17.83327_f32,
    18.134155_f32,
    18.41471_f32,
    18.674232_f32,
    18.912073_f32,
    19.12764_f32,
    19.32039_f32,
    19.489845_f32,
    19.635582_f32,
    19.757233_f32,
    19.854498_f32,
    19.92713_f32,
    19.97495_f32,
    19.997837_f32,
    19.995735_f32,
    19.96865_f32,
    19.916649_f32,
    19.839859_f32,
    19.738476_f32,
    19.612753_f32,
    19.463001_f32,
    19.289597_f32,
    19.092974_f32,
    18.873623_f32,
    18.632093_f32,
    18.368988_f32,
    18.084965_f32,
    17.780731_f32,
    17.457052_f32,
    17.114733_f32,
    16.754631_f32,
    16.377647_f32,
    15.984721_f32,
    15.576838_f32,
    15.155014_f32,
    14.720305_f32,
    14.273799_f32,
    13.81661_f32,
    13.349881_f32,
    12.87478_f32,
    12.392493_f32,
    11.904226_f32,
    11.411201_f32,
    10.914646_f32,
    10.415807_f32,
    9.915928_f32,
    9.416259_f32,
    8.918049_f32,
    8.422544_f32,
    7.93098_f32,
    7.444589_f32,
    6.964585_f32,
    6.492168_f32,
    6.028518_f32,
    5.5747957_f32,
    5.1321335_f32,
    4.7016387_f32,
    4.2843866_f32,
    3.881421_f32,
    3.4937487_f32,
    3.1223383_f32,
    2.7681189_f32,
    2.4319751_f32,
    2.1147475_f32,
    1.8172289_f32,
    1.540163_f32,
    1.2842423_f32,
    1.0501064_f32,
    0.83834064_f32,
    0.6494742_f32,
    0.48397925_f32,
    0.3422694_f32,
    0.22469883_f32,
    0.13156141_f32,
    0.06308997_f32,
    0.019455612_f32,
    7.6742435E-4_f32,
    0.00707211_f32,
    0.038353913_f32,
    0.09453464_f32,
    0.17547387_f32,
    0.2809693_f32,
    0.41075724_f32,
    0.5645133_f32,
    0.7418532_f32,
    0.9423336_f32,
    1.1654534_f32,
    1.410655_f32,
    1.6773256_f32,
    1.9647985_f32,
    2.272355_f32,
    2.599227_f32,
    2.9445968_f32,
    3.3076015_f32,
    3.6873336_f32,
    4.0828443_f32,
    4.4931445_f32,
    4.917209_f32,
    5.353978_f32,
    5.80236_f32,
    6.2612333_f32,
    6.7294517_f32,
    7.205845_f32,
    7.6892223_f32,
    8.178375_f32,
    8.672081_f32,
    9.169106_f32,
    9.668208_f32,
    10.168139_f32,
];

/// `Globals.exitToGameOver`.
pub const EXIT_TO_GAME_OVER: i32 = 5;

/// Win camera after `winGame()` (`main.lua`).
pub const WIN_CAMERA_START: (i32, i32) = (54, 5);

/// Sea pan cameras (`main.lua` kGameWinState).
pub const WIN_CAMERA_PAN: [(i32, i32); 3] = [(240, 47), (217, 22), (187, 17)];

/// Credits sprite top-left after `moveTo(48, 68)` (`main.lua` overrides init).
pub const CREDITS_POS: (i32, i32) = (48, 68);
pub const CREDITS_W: i32 = 307;
pub const CREDITS_H: i32 = 96;
const CREDITS_TEXT_RECT: (i32, i32, i32, i32) = (18, 19, 265, 61);

/// Highscore panel after `hstable:moveTo(170, 0)` (`setCenter(0,0)`, size 232×240).
pub const HS_PANEL_POS: (i32, i32) = (170, 0);
/// Catalog / local board rows — screen coords.
///
/// Lua `highscoretable:draw` calls `drawTextAligned(…, self.x - 128, self.y + 64)`
/// **inside** `sprite:draw`, where the CTM is already translated to the sprite
/// top-left. With panel at `(170,0)` that local `(42,64)` lands at screen
/// `(212,64)`. Treating `(42,64)` as screen coords put the text under the
/// samurai on the left (and hid rows on light `endbg` paper).
pub const HS_TABLE_TEXT_POS: (i32, i32) = (212, 64);
/// Last-score line — same CTM gotcha as [`HS_TABLE_TEXT_POS`] (`y + 188`).
pub const HS_PLAYER_TEXT_POS: (i32, i32) = (212, 188);

/// Tick gate for each credit card while `gameoverScreen == 3` (`main.lua`).
///
/// Numbers only: the card prose is authored content, extracted at runtime from
/// the user's `main.luac` (see [`crate::credits_from_main_luac`]).
pub const CREDIT_TICKS: &[i32] = &[610, 810, 1010, 1210, 1410, 1610, 1810, 2010];

/// Open/close wipe / hs panel: 10 fps → 0.1s per cell.
const ANIM_FRAME_DT: f32 = 0.1;

#[derive(Debug, Clone)]
pub struct EndingRuntime {
    /// `gameoverScreen` while winning / after jump to score (0..=4).
    pub gameover_screen: u8,
    /// 20 Hz ticks while in Win or Score (`main.lua` `ticks`).
    pub ticks: i32,
    /// Remaining blood captured at `winGame` (`score = player.blood`).
    pub ending_score: i32,
    /// Player sprite hidden during win cinema (`player:setVisible(false)`).
    pub player_visible: bool,
    /// Credits scroller active (`creditscroller ~= nil`).
    pub credits_active: bool,
    pub credits_text: String,
    pub credits_next_text: String,
    /// 1-based wipe frame (Lua `currentFrame`).
    pub credits_frame: usize,
    /// Open anim: frame indices 1..=14, advancing via `anim_counter`.
    pub credits_anim_index: usize,
    pub credits_anim_counter: f32,
    pub credits_animating: bool,
    /// Score-screen `winbackground` LUT index (1-based in Lua).
    pub win_bg_t: usize,
    /// Highscore table created (`hstable ~= nil`).
    pub hs_table: bool,
    /// 1-based `highscore.pdt` frame.
    pub hs_frame: usize,
    pub hs_anim_index: usize,
    pub hs_anim_counter: f32,
    pub hs_animating: bool,
    /// `"last score" + dots + score`.
    pub hs_player_text: String,
    /// Catalog-style top rows (`name…..value\n`), filled from the local board.
    pub hs_table_text: String,
}

impl Default for EndingRuntime {
    fn default() -> Self {
        Self {
            gameover_screen: 0,
            ticks: 0,
            ending_score: 0,
            player_visible: true,
            credits_active: false,
            credits_text: String::new(),
            credits_next_text: String::new(),
            credits_frame: 1,
            credits_anim_index: 0,
            credits_anim_counter: 0.0,
            credits_animating: false,
            win_bg_t: 0,
            hs_table: false,
            hs_frame: 1,
            hs_anim_index: 0,
            hs_anim_counter: 0.0,
            hs_animating: false,
            hs_player_text: String::new(),
            hs_table_text: String::new(),
        }
    }
}

impl EndingRuntime {
    pub fn reset_for_win(&mut self, blood: i32) {
        *self = Self {
            ending_score: blood,
            player_visible: false,
            ..Self::default()
        };
        self.player_visible = false;
        self.ending_score = blood;
    }

    pub fn reset_for_score(&mut self) {
        let score = self.ending_score;
        self.gameover_screen = 4;
        self.ticks = 0;
        self.credits_active = false;
        self.credits_text.clear();
        self.credits_next_text.clear();
        self.credits_animating = false;
        self.win_bg_t = 0;
        self.hs_table = false;
        self.hs_frame = 1;
        self.hs_anim_index = 0;
        self.hs_anim_counter = 0.0;
        self.hs_animating = false;
        self.hs_player_text.clear();
        self.hs_table_text.clear();
        self.ending_score = score;
        self.player_visible = false;
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Build Lua `playerText` with period padding (`highscoretable.lua`).
pub fn format_last_score_line(font: Option<&Font>, score: i32) -> String {
    format_padded_line(font, "last score", score)
}

/// Advance credits open anim (`credits:update`, 10 fps through wipe 1..=14).
pub fn tick_credits_anim(end: &mut EndingRuntime, dt: f32, wipe_len: usize) {
    // Lua restarts open whenever `nextText` changes (win path never sets closing).
    if end.credits_next_text != end.credits_text {
        end.credits_text = end.credits_next_text.clone();
        end.credits_anim_index = 0;
        end.credits_anim_counter = ANIM_FRAME_DT;
        end.credits_animating = true;
        end.credits_frame = 1;
    }
    if !end.credits_animating {
        return;
    }
    end.credits_anim_counter -= dt;
    while end.credits_anim_counter <= 0.0 && end.credits_animating {
        end.credits_anim_index += 1;
        let frame_1based = end.credits_anim_index + 1; // index 0 → frame 1
        if frame_1based > wipe_len.max(1) || end.credits_anim_index >= 14 {
            end.credits_animating = false;
            end.credits_frame = wipe_len.max(1).min(14);
            break;
        }
        end.credits_frame = frame_1based.min(wipe_len.max(1));
        end.credits_anim_counter += ANIM_FRAME_DT;
    }
}

/// Advance highscore panel open 1→4 @ 10 fps.
pub fn tick_hs_anim(end: &mut EndingRuntime, dt: f32, n_frames: usize) {
    if !end.hs_animating {
        return;
    }
    end.hs_anim_counter -= dt;
    while end.hs_anim_counter <= 0.0 && end.hs_animating {
        end.hs_anim_index += 1;
        let frame_1based = end.hs_anim_index + 1;
        if frame_1based > n_frames.max(1) || end.hs_anim_index >= 4 {
            end.hs_animating = false;
            end.hs_frame = n_frames.max(1).min(4);
            break;
        }
        end.hs_frame = frame_1based.min(n_frames.max(1));
        end.hs_anim_counter += ANIM_FRAME_DT;
    }
}

pub fn start_hs_open(end: &mut EndingRuntime) {
    end.hs_table = true;
    end.hs_anim_index = 0;
    end.hs_frame = 1;
    end.hs_anim_counter = ANIM_FRAME_DT;
    end.hs_animating = true;
}

pub fn credits_show(end: &mut EndingRuntime, text: &str) {
    end.credits_next_text = text.to_string();
}

/// Advance `winbackground` LUT index one display tick (`winbackground:update`).
pub fn tick_winbackground(t: &mut usize) {
    let lut_len = WIN_BG_LUT.len();
    if lut_len == 0 {
        return;
    }
    // Lua: if t > #lut then t = 1; t = t + 1  (1-based).
    *t += 1;
    if *t > lut_len {
        *t = 1;
    }
}

/// Draw `winbackground`: LUT wave chords + `endBG` at (1,1).
pub fn draw_winbackground(fb: &mut Framebuffer, end_bg: Option<&Bitmap>, t: usize) {
    let lut_len = WIN_BG_LUT.len();
    fb.clear(true);
    if lut_len == 0 || t == 0 {
        if let Some(img) = end_bg {
            img.blit(fb, 1, 1);
        }
        return;
    }
    let x_center = 98i32;
    let t0 = t as i32; // 1-based
    for y in 85..=178 {
        let dy = (178 - y) as f32;
        let ht = (dy * dy * dy * 0.002).floor() as i32;
        // Lua: lut[(t + ht) % (#lut - 1) + 1]
        let mod_base = (lut_len - 1).max(1) as i32;
        let idx_1based = ((t0 + ht).rem_euclid(mod_base) as usize) + 1;
        let lut_v = WIN_BG_LUT
            .get(idx_1based.saturating_sub(1))
            .copied()
            .unwrap_or(0.0);
        let w = (lut_v + 14.0).floor() as i32;
        fb.hline(x_center - w, x_center + w, y, false); // white chords on black
    }
    if let Some(img) = end_bg {
        img.blit(fb, 1, 1);
    }
}

/// Draw credits panel with wipe-cell mask (`credits:draw`).
pub fn draw_credits(
    fb: &mut Framebuffer,
    end: &EndingRuntime,
    font: Option<&Font>,
    wipe: Option<&ImageTable>,
) {
    if !end.credits_active {
        return;
    }
    let mut canvas = Bitmap::empty(CREDITS_W as u32, CREDITS_H as u32);
    // Black field (empty() is white — paint black).
    for y in 0..CREDITS_H as u32 {
        for x in 0..CREDITS_W as u32 {
            canvas.set_pixel(x, y, false, true);
        }
    }
    if let Some(font) = font {
        draw_text_centered_in_rect(
            font,
            &mut canvas,
            &end.credits_text,
            CREDITS_TEXT_RECT.0,
            CREDITS_TEXT_RECT.1,
            CREDITS_TEXT_RECT.2,
            CREDITS_TEXT_RECT.3,
            1,
        );
    }
    let mask = wipe.and_then(|t| {
        let i = end.credits_frame.saturating_sub(1);
        t.get(i)
    });
    let (dx, dy) = CREDITS_POS;
    for y in 0..CREDITS_H as u32 {
        for x in 0..CREDITS_W as u32 {
            // Playdate `setMaskImage`: opaque mask pixels reveal the canvas.
            let visible = match mask {
                Some(m) => {
                    let mx = x.min(m.width.saturating_sub(1));
                    let my = y.min(m.height.saturating_sub(1));
                    // `wipe.pdt` has no alpha — white ink grows as the iris opens.
                    // Playdate mask: white = reveal the credits canvas.
                    m.color_at(mx, my)
                }
                // No wipe asset: show full panel once text exists.
                None => !end.credits_text.is_empty(),
            };
            if !visible {
                continue;
            }
            let white = canvas.color_at(x, y);
            let px = dx + x as i32;
            let py = dy + y as i32;
            if px >= 0 && py >= 0 && px < SCREEN_WIDTH as i32 && py < SCREEN_HEIGHT as i32 {
                fb.set_pixel(px as u32, py as u32, !white);
            }
        }
    }
}

fn draw_text_centered_in_rect(
    font: &Font,
    canvas: &mut Bitmap,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    tracking: i32,
) {
    // Layout lines like draw_text_in_rect, but center each line horizontally.
    // Glyphs are black; we want white text → store white=true on canvas.
    let line_h = font.glyph_height as i32;
    let mut cy = y;
    let bottom = y + height;
    let mut lines: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        let words: Vec<&str> = paragraph.split_whitespace().collect();
        if words.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in words {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if font.measure_text(&candidate, tracking) <= width {
                line = candidate;
            } else {
                if !line.is_empty() {
                    lines.push(line);
                }
                line = word.to_string();
            }
        }
        lines.push(line);
    }
    for line in lines {
        if cy + line_h > bottom {
            break;
        }
        if !line.is_empty() {
            let w = font.measure_text(&line, tracking);
            let lx = x + (width - w) / 2;
            draw_white_text(font, canvas, lx, cy, &line, tracking);
        }
        cy += line_h;
    }
}

fn draw_white_text(font: &Font, canvas: &mut Bitmap, x: i32, y: i32, text: &str, tracking: i32) {
    let chars: Vec<char> = text.chars().collect();
    let mut cx = x;
    for (i, ch) in chars.iter().enumerate() {
        let cp = *ch as u32;
        let Some(glyph) = font.glyphs.get(&cp) else {
            continue;
        };
        // `headerwhite` glyph ink is the white color bit (same as lifebar blit).
        let img = &glyph.image;
        for gy in 0..img.height {
            for gx in 0..img.width {
                if !img.opaque_at(gx, gy) || !img.color_at(gx, gy) {
                    continue;
                }
                let px = cx + gx as i32;
                let py = y + gy as i32;
                if px >= 0 && py >= 0 {
                    canvas.set_pixel(px as u32, py as u32, true, true);
                }
            }
        }
        let mut adv = glyph.advance as i32;
        if let Some(next) = chars.get(i + 1) {
            let next_cp = *next as u32;
            if let Some(k) = glyph.kerning.get(&next_cp) {
                adv += *k as i32;
            }
            adv += tracking;
        }
        cx += adv;
    }
}

/// Draw highscore panel + board rows + last-score line (`highscoretable:draw`).
///
/// Lua uses plain `drawTextAligned` with `hsfont` (`monoblack`) — black ink,
/// no invert. Text sits on the light `highscore.pdt` panel, so Copy keeps glyphs
/// visible; Inverted would paint white-on-cream and disappear.
pub fn draw_highscore_panel(
    fb: &mut Framebuffer,
    end: &EndingRuntime,
    hs_table: Option<&ImageTable>,
    font: Option<&Font>,
) {
    if !end.hs_table {
        return;
    }
    let (px, py) = HS_PANEL_POS;
    if let Some(table) = hs_table {
        let idx = end.hs_frame.saturating_sub(1);
        if let Some(cell) = table.get(idx) {
            cell.blit(fb, px, py);
        }
    }
    // Text only once open frame (== 4 in Lua `animFrame == 4`).
    if end.hs_frame >= 4 {
        if let Some(font) = font {
            if !end.hs_table_text.is_empty() {
                let (tx, ty) = HS_TABLE_TEXT_POS;
                draw_hs_multiline(fb, font, tx, ty, &end.hs_table_text);
            }
            if !end.hs_player_text.is_empty() {
                let (tx, ty) = HS_PLAYER_TEXT_POS;
                draw_hs_score_line(fb, font, tx, ty, &end.hs_player_text);
            }
        }
    }
}

fn draw_hs_multiline(fb: &mut Framebuffer, font: &Font, x: i32, y: i32, text: &str) {
    let line_h = font.glyph_height as i32;
    let mut cy = y;
    for line in text.split('\n') {
        if !line.is_empty() {
            draw_hs_score_line(fb, font, x, cy, line);
        }
        cy += line_h;
    }
}

/// Draw a padded score row with the numeric suffix right-aligned to `x + HS_DOT_PAD`.
///
/// Prefix (name + periods) stays left-aligned. The value is placed so its advance
/// ends on the shared column — thin digits (`181`) no longer sit 2px right of `245`.
fn draw_hs_score_line(fb: &mut Framebuffer, font: &Font, x: i32, y: i32, text: &str) {
    let (prefix, number) = split_score_line(text);
    if number.is_empty() {
        draw_hs_glyphs(fb, font, x, y, text);
        return;
    }
    draw_hs_glyphs(fb, font, x, y, prefix);
    let num_w = font.measure_text(number, 1);
    let num_x = x + HS_DOT_PAD - num_w;
    draw_hs_glyphs(fb, font, num_x, y, number);
}

fn draw_hs_glyphs(fb: &mut Framebuffer, font: &Font, x: i32, y: i32, text: &str) {
    let chars: Vec<char> = text.chars().collect();
    let mut cx = x;
    for (i, ch) in chars.iter().enumerate() {
        let cp = *ch as u32;
        let Some(glyph) = font.glyphs.get(&cp) else {
            continue;
        };
        glyph.image.blit_mode(fb, cx, y, DrawMode::Copy);
        let mut adv = glyph.advance as i32;
        if let Some(next) = chars.get(i + 1) {
            let next_cp = *next as u32;
            if let Some(k) = glyph.kerning.get(&next_cp) {
                adv += *k as i32;
            }
            adv += 1; // Lua `setFontTracking(1)`
        }
        cx += adv;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdi::decode_pdt;
    use crate::pft::decode_pft;

    /// Text is drawn in sprite-local coords translated by the panel origin —
    /// not as bare `(self.x - 128)` screen coords (that lands under the samurai).
    #[test]
    fn hs_text_sits_inside_panel() {
        let (px, _) = HS_PANEL_POS;
        assert!(
            HS_TABLE_TEXT_POS.0 > px && HS_TABLE_TEXT_POS.0 < px + 232,
            "table text x={} must be inside panel starting at {}",
            HS_TABLE_TEXT_POS.0,
            px
        );
        assert!(
            HS_PLAYER_TEXT_POS.0 > px && HS_PLAYER_TEXT_POS.0 < px + 232,
            "last-score x={} must be inside panel starting at {}",
            HS_PLAYER_TEXT_POS.0,
            px
        );
        assert_eq!(HS_TABLE_TEXT_POS, (212, 64));
        assert_eq!(HS_PLAYER_TEXT_POS, (212, 188));
    }

    fn load_wipe() -> Option<ImageTable> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../www/assets/demo/wipe.pdt");
        let data = std::fs::read(path).ok()?;
        decode_pdt(&data).ok()
    }

    fn load_headerwhite() -> Option<Font> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../www/assets/demo/headerwhite.pft"
        );
        let data = std::fs::read(path).ok()?;
        decode_pft(&data).ok()
    }

    fn panel_ink_counts(fb: &Framebuffer) -> (usize, usize) {
        let (dx, dy) = CREDITS_POS;
        let mut black = 0usize;
        let mut white = 0usize;
        for y in 0..CREDITS_H {
            for x in 0..CREDITS_W {
                if fb.get_pixel((dx + x) as u32, (dy + y) as u32) {
                    black += 1;
                } else {
                    white += 1;
                }
            }
        }
        (black, white)
    }

    /// Wipe white-ink mask + headerwhite glyph polarity must reveal thank-you text.
    #[test]
    fn credits_draw_shows_text_through_wipe() {
        let Some(wipe) = load_wipe() else {
            return;
        };
        let Some(font) = load_headerwhite() else {
            return;
        };
        let mut end = EndingRuntime::default();
        end.credits_active = true;
        credits_show(&mut end, "Test card\nsecond line");
        tick_credits_anim(&mut end, 0.0, wipe.len());
        end.credits_animating = false;
        end.credits_frame = wipe.len().min(14);

        let mut fb = Framebuffer::default();
        // Paper background so unrevealed panel stays white.
        for y in 0..SCREEN_HEIGHT {
            for x in 0..SCREEN_WIDTH {
                fb.set_pixel(x, y, false);
            }
        }
        draw_credits(&mut fb, &end, Some(&font), Some(&wipe));
        let (black, white) = panel_ink_counts(&fb);
        assert!(
            black > 1000 && white > 100,
            "open wipe must paint black field + white glyphs, got black={black} white={white}"
        );
        assert!(
            end.credits_text.contains("Test card"),
            "first card text must be installed"
        );
    }

    /// Early wipe frame reveals far less of the panel than the final frame.
    #[test]
    fn credits_wipe_iris_grows() {
        let Some(wipe) = load_wipe() else {
            return;
        };
        let Some(font) = load_headerwhite() else {
            return;
        };
        let mut end = EndingRuntime::default();
        end.credits_active = true;
        credits_show(&mut end, "Test card\nsecond line");
        tick_credits_anim(&mut end, 0.0, wipe.len());
        end.credits_animating = false;

        let paint = |frame: usize| {
            let mut e = end.clone();
            e.credits_frame = frame;
            let mut fb = Framebuffer::default();
            for y in 0..SCREEN_HEIGHT {
                for x in 0..SCREEN_WIDTH {
                    fb.set_pixel(x, y, false);
                }
            }
            draw_credits(&mut fb, &e, Some(&font), Some(&wipe));
            panel_ink_counts(&fb).0 // black = revealed panel ink
        };

        let early = paint(1);
        let late = paint(wipe.len().min(14));
        assert!(
            late > early * 2,
            "wipe iris must grow: frame1 black={early}, open black={late}"
        );
    }
}
