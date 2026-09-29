//! Playdate `.pft` font decoder.
//!
//! Layout matches Zipper's shipped fonts and the cranksters reverse-engineering
//! notes, with two empirical corrections vs the published doc:
//! - page-list `uint32` values are **end** offsets relative to the page base
//! - each glyph starts with a reserved `uint16` (0) before the advance/kern header
//! - glyph offset table entries are likewise **end** offsets relative to glyph base

use std::collections::HashMap;

use crate::bitmap::Bitmap;
use crate::framebuffer::Framebuffer;
use crate::pdi::DecodeError;

const PFT_MAGIC: &[u8; 12] = b"Playdate FNT";
const FLAG_COMPRESSED: u32 = 0x8000_0000;

fn read_u16(data: &[u8], off: usize) -> Result<u16, DecodeError> {
    let b = data.get(off..off + 2).ok_or(DecodeError::Truncated)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn read_u32(data: &[u8], off: usize) -> Result<u32, DecodeError> {
    let b = data.get(off..off + 4).ok_or(DecodeError::Truncated)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_i8(data: &[u8], off: usize) -> Result<i8, DecodeError> {
    Ok(*data.get(off).ok_or(DecodeError::Truncated)? as i8)
}

/// One decoded glyph: advance width, optional kerning pairs, and image cell.
#[derive(Clone, Debug)]
pub struct Glyph {
    pub advance: u8,
    /// Kerning against the following codepoint (same-page short table + long).
    pub kerning: HashMap<u32, i8>,
    pub image: Bitmap,
}

/// Decoded Playdate font.
#[derive(Clone, Debug)]
pub struct Font {
    pub glyph_width: u8,
    pub glyph_height: u8,
    /// Default tracking from the file (pixels between glyphs).
    pub tracking: u16,
    pub glyphs: HashMap<u32, Glyph>,
}

impl Font {
    /// Draw `text` at `(x, y)` with absolute tracking (Playdate `setFontTracking`).
    ///
    /// Missing glyphs are skipped (advance 0). Kerning from the font is applied
    /// between consecutive characters, matching `gfx.drawText`.
    pub fn draw_text(&self, fb: &mut Framebuffer, x: i32, y: i32, text: &str, tracking: i32) {
        let chars: Vec<char> = text.chars().collect();
        let mut cx = x;
        for (i, ch) in chars.iter().enumerate() {
            let cp = *ch as u32;
            let Some(glyph) = self.glyphs.get(&cp) else {
                continue;
            };
            glyph.image.blit(fb, cx, y);
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

    /// Advance width of `text` with the same tracking/kerning as [`Self::draw_text`].
    pub fn measure_text(&self, text: &str, tracking: i32) -> i32 {
        let chars: Vec<char> = text.chars().collect();
        let mut w = 0i32;
        for (i, ch) in chars.iter().enumerate() {
            let cp = *ch as u32;
            let Some(glyph) = self.glyphs.get(&cp) else {
                continue;
            };
            let mut adv = glyph.advance as i32;
            if let Some(next) = chars.get(i + 1) {
                let next_cp = *next as u32;
                if let Some(k) = glyph.kerning.get(&next_cp) {
                    adv += *k as i32;
                }
                adv += tracking;
            }
            w += adv;
        }
        w
    }

    /// Word-wrapped text in a rect (`gfx.drawTextInRect`).
    ///
    /// `leading` is extra pixels between lines (`-1` in dialog.lua → default line
    /// gap of one glyph height). Clips once the next line would leave the rect.
    pub fn draw_text_in_rect(
        &self,
        fb: &mut Framebuffer,
        text: &str,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        tracking: i32,
        leading: i32,
    ) {
        let line_h = self.glyph_height as i32 + if leading < 0 { 0 } else { leading };
        let mut cy = y;
        let bottom = y + height;
        for paragraph in text.split('\n') {
            let words: Vec<&str> = paragraph.split_whitespace().collect();
            if words.is_empty() {
                cy += line_h;
                if cy + self.glyph_height as i32 > bottom {
                    return;
                }
                continue;
            }
            let mut line = String::new();
            for word in words {
                let candidate = if line.is_empty() {
                    word.to_string()
                } else {
                    format!("{line} {word}")
                };
                if self.measure_text(&candidate, tracking) <= width {
                    line = candidate;
                } else {
                    if !line.is_empty() {
                        if cy + self.glyph_height as i32 > bottom {
                            return;
                        }
                        self.draw_text(fb, x, cy, &line, tracking);
                        cy += line_h;
                    }
                    line = word.to_string();
                    // Overlong single word: still draw (Playdate clips).
                    if self.measure_text(&line, tracking) > width {
                        if cy + self.glyph_height as i32 > bottom {
                            return;
                        }
                        self.draw_text(fb, x, cy, &line, tracking);
                        cy += line_h;
                        line.clear();
                    }
                }
            }
            if !line.is_empty() {
                if cy + self.glyph_height as i32 > bottom {
                    return;
                }
                self.draw_text(fb, x, cy, &line, tracking);
                cy += line_h;
            }
        }
    }
}

/// Decode a `.pft` font file into a glyph map.
pub fn decode_pft(data: &[u8]) -> Result<Font, DecodeError> {
    if data.len() < 16 || &data[0..12] != PFT_MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let flags = read_u32(data, 12)?;
    let raw = if flags & FLAG_COMPRESSED != 0 {
        if data.len() < 32 {
            return Err(DecodeError::Truncated);
        }
        // compressed header: decompressed size, max glyph w, max glyph h
        let _size = read_u32(data, 16)?;
        miniz_oxide::inflate::decompress_to_vec_zlib(&data[32..]).map_err(|_| DecodeError::Inflate)?
    } else {
        data[16..].to_vec()
    };

    if raw.len() < 68 {
        return Err(DecodeError::Truncated);
    }
    let glyph_width = raw[0];
    let glyph_height = raw[1];
    let tracking = read_u16(&raw, 2)?;
    let page_flags = &raw[4..68];
    let pages: Vec<u32> = (0..512u32)
        .filter(|i| (page_flags[*i as usize / 8] >> (*i as usize % 8)) & 1 == 1)
        .collect();
    if pages.is_empty() {
        return Err(DecodeError::EmptyTable);
    }

    let page_off_table = 68usize;
    let page_base = page_off_table + pages.len() * 4;
    if raw.len() < page_base {
        return Err(DecodeError::Truncated);
    }
    // End offsets relative to `page_base`.
    let mut page_ends = Vec::with_capacity(pages.len());
    for i in 0..pages.len() {
        page_ends.push(read_u32(&raw, page_off_table + i * 4)? as usize);
    }

    let mut glyphs: HashMap<u32, Glyph> = HashMap::new();
    for (pi, page_idx) in pages.iter().enumerate() {
        let page_start = page_base + if pi == 0 { 0 } else { page_ends[pi - 1] };
        let page_end = page_base + page_ends[pi];
        if page_end > raw.len() || page_start + 36 > page_end {
            return Err(DecodeError::Truncated);
        }
        let page = &raw[page_start..page_end];
        // page header: u24 reserved, u8 nglyphs, 32-byte usage flags
        let _nglyphs = page[3];
        let glyph_flags = &page[4..36];
        let present: Vec<u32> = (0..256u32)
            .filter(|i| (glyph_flags[*i as usize / 8] >> (*i as usize % 8)) & 1 == 1)
            .collect();
        if present.is_empty() {
            continue;
        }

        let off_table = 36usize;
        let need = off_table + present.len() * 2;
        if page.len() < need {
            return Err(DecodeError::Truncated);
        }
        let mut glyph_ends = Vec::with_capacity(present.len());
        for i in 0..present.len() {
            glyph_ends.push(read_u16(page, off_table + i * 2)? as usize);
        }
        let glyph_base = off_table + present.len() * 2;

        // Most Zipper HUD fonts start each glyph with a reserved `u16` (0).
        // `Zipper.pft` omits it and begins at advance. Detect from the first glyph.
        let leading_reserved = {
            let first_end = glyph_base + glyph_ends[0];
            first_end > glyph_base + 2 && read_u16(page, glyph_base)? == 0
        };

        for (gi, local_cp) in present.iter().enumerate() {
            let g_start = glyph_base + if gi == 0 { 0 } else { glyph_ends[gi - 1] };
            let g_end = glyph_base + glyph_ends[gi];
            let hdr = if leading_reserved { 6 } else { 4 };
            if g_end > page.len() || g_start + hdr > g_end {
                // Stub / truncated trailing glyph (seen as 2-byte pad on '~').
                continue;
            }
            // Optional reserved u16 (0) + advance u8 + nshort u8 + nlong u16
            let (advance, nshort, nlong, mut pos) = if leading_reserved {
                let _reserved = read_u16(page, g_start)?;
                (
                    page[g_start + 2],
                    page[g_start + 3] as usize,
                    read_u16(page, g_start + 4)? as usize,
                    g_start + 6,
                )
            } else {
                (
                    page[g_start],
                    page[g_start + 1] as usize,
                    read_u16(page, g_start + 2)? as usize,
                    g_start + 4,
                )
            };
            // Kern pad aligns to 4 relative to the page, not glyph start.
            let mut kerning: HashMap<u32, i8> = HashMap::new();
            for _ in 0..nshort {
                if pos + 2 > g_end {
                    return Err(DecodeError::Truncated);
                }
                let other = page[pos] as u32;
                let kern = read_i8(page, pos + 1)?;
                kerning.insert((*page_idx << 8) | other, kern);
                pos += 2;
            }
            if pos % 4 != 0 {
                pos += 4 - (pos % 4);
            }
            for _ in 0..nlong {
                if pos + 4 > g_end {
                    return Err(DecodeError::Truncated);
                }
                let other = (page[pos] as u32)
                    | ((page[pos + 1] as u32) << 8)
                    | ((page[pos + 2] as u32) << 16);
                let kern = read_i8(page, pos + 3)?;
                kerning.insert(other, kern);
                pos += 4;
            }
            if pos >= g_end {
                continue;
            }
            // Always mask padding transparent — even cells with no alpha plane
            // (`flags & 3 == 0`, e.g. 'I') would otherwise paint opaque-white
            // margins over the lifebar chrome.
            let image = decode_font_cell(page, pos)?;

            let cp = (*page_idx << 8) | *local_cp;
            glyphs.insert(
                cp,
                Glyph {
                    advance,
                    kerning,
                    image,
                },
            );
        }
    }

    if glyphs.is_empty() {
        return Err(DecodeError::EmptyTable);
    }

    Ok(Font {
        glyph_width,
        glyph_height,
        tracking,
        glyphs,
    })
}

/// Like [`decode_cell`], but padding outside the clip is always transparent.
fn decode_font_cell(data: &[u8], offset: usize) -> Result<Bitmap, DecodeError> {
    if data.len() < offset + 16 {
        return Err(DecodeError::Truncated);
    }
    let clip_w = read_u16(data, offset)? as u32;
    let clip_h = read_u16(data, offset + 2)? as u32;
    let stride = read_u16(data, offset + 4)? as usize;
    let clip_left = read_u16(data, offset + 6)? as u32;
    let clip_right = read_u16(data, offset + 8)? as u32;
    let clip_top = read_u16(data, offset + 10)? as u32;
    let clip_bottom = read_u16(data, offset + 12)? as u32;
    let flags = read_u16(data, offset + 14)?;
    let has_alpha = flags & 0x3 != 0;

    let color_size = stride
        .checked_mul(clip_h as usize)
        .ok_or(DecodeError::BadCell)?;
    let mut pos = offset + 16;
    if data.len() < pos + color_size {
        return Err(DecodeError::Truncated);
    }
    let color_data = &data[pos..pos + color_size];
    pos += color_size;
    let alpha_data = if has_alpha {
        if data.len() < pos + color_size {
            return Err(DecodeError::Truncated);
        }
        let a = &data[pos..pos + color_size];
        Some(a)
    } else {
        None
    };

    let full_w = clip_left + clip_w + clip_right;
    let full_h = clip_top + clip_h + clip_bottom;
    if full_w == 0 || full_h == 0 {
        return Err(DecodeError::BadCell);
    }
    if clip_w == 0 || clip_h == 0 {
        return Ok(Bitmap::transparent(full_w, full_h));
    }

    let mut bmp = Bitmap::with_alpha(full_w, full_h);
    for cy in 0..clip_h {
        let row_off = cy as usize * stride;
        for cx in 0..clip_w {
            let byte_i = row_off + (cx as usize / 8);
            let bit = 7 - (cx as usize % 8);
            if byte_i >= color_data.len() {
                return Err(DecodeError::BadCell);
            }
            let white = (color_data[byte_i] >> bit) & 1 == 1;
            let opaque = match alpha_data {
                Some(a) => (a[byte_i] >> bit) & 1 == 1,
                None => true,
            };
            bmp.set_pixel(clip_left + cx, clip_top + cy, white, opaque);
        }
    }
    Ok(bmp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_headerwhite() -> Option<Font> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ref/Zipper.pdx/Fonts/headerwhite.pft"
        );
        let data = std::fs::read(path).ok()?;
        decode_pft(&data).ok()
    }

    #[test]
    fn headerwhite_decodes_life_glyphs() {
        let Some(font) = load_headerwhite() else {
            return;
        };
        assert_eq!(font.glyph_width, 10);
        assert_eq!(font.glyph_height, 15);
        assert_eq!(font.tracking, 0);
        for c in "LIFE:0123456789".chars() {
            let g = font
                .glyphs
                .get(&(c as u32))
                .unwrap_or_else(|| panic!("missing {c}"));
            assert_eq!(g.image.height, 15, "glyph {c} height");
            assert!(g.advance > 0, "glyph {c} advance");
        }
        // Spot-check shapes / advances against the empirical probe.
        assert_eq!(font.glyphs[&(b'L' as u32)].advance, 5);
        assert_eq!(font.glyphs[&(b'I' as u32)].advance, 2);
        assert_eq!(font.glyphs[&(b'0' as u32)].advance, 6);
        assert_eq!(font.glyphs[&(b'1' as u32)].advance, 3);
        // 'I' has no alpha plane in the file; padding must still be transparent.
        let i = &font.glyphs[&(b'I' as u32)].image;
        assert!(i.alpha.is_some(), "font glyphs always masked");
        assert!(!i.opaque_at(9, 0), "I padding must be transparent");
        assert!(i.opaque_at(0, 4), "I stem must be opaque");
    }

    #[test]
    fn all_zipper_fonts_decode() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../ref/Zipper.pdx/Fonts");
        if !std::path::Path::new(dir).is_dir() {
            return;
        }
        for name in [
            "headerwhite.pft",
            "headerblack.pft",
            "headerwhite_bold.pft",
            "monoblack.pft",
            "Zipper.pft",
        ] {
            let data = std::fs::read(format!("{dir}/{name}")).unwrap_or_else(|e| panic!("{name}: {e}"));
            let font = decode_pft(&data).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(
                font.glyphs.len() >= 90,
                "{name} expected >=90 glyphs, got {}",
                font.glyphs.len()
            );
        }
    }
}
