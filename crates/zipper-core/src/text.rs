//! Tiny 3×5 debug font (uppercase). Real `.pft` fonts come later.

use crate::framebuffer::Framebuffer;

fn glyph(c: u8) -> Option<[u8; 5]> {
    Some(match c {
        b' ' => [0, 0, 0, 0, 0],
        b'/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        b'0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        b'1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        b'2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        b'3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        b'4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        b'5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        b'6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        b'7' => [0b111, 0b001, 0b001, 0b001, 0b001],
        b'8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        b'9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        b'A' | b'a' => [0b010, 0b101, 0b111, 0b101, 0b101],
        b'B' | b'b' => [0b110, 0b101, 0b110, 0b101, 0b110],
        b'C' | b'c' => [0b011, 0b100, 0b100, 0b100, 0b011],
        b'D' | b'd' => [0b110, 0b101, 0b101, 0b101, 0b110],
        b'E' | b'e' => [0b111, 0b100, 0b110, 0b100, 0b111],
        b'F' | b'f' => [0b111, 0b100, 0b110, 0b100, 0b100],
        b'G' | b'g' => [0b011, 0b100, 0b101, 0b101, 0b011],
        b'H' | b'h' => [0b101, 0b101, 0b111, 0b101, 0b101],
        b'I' | b'i' => [0b111, 0b010, 0b010, 0b010, 0b111],
        b'J' | b'j' => [0b001, 0b001, 0b001, 0b101, 0b010],
        b'K' | b'k' => [0b101, 0b101, 0b110, 0b101, 0b101],
        b'L' | b'l' => [0b100, 0b100, 0b100, 0b100, 0b111],
        b'M' | b'm' => [0b101, 0b111, 0b111, 0b101, 0b101],
        b'N' | b'n' => [0b101, 0b111, 0b111, 0b111, 0b101],
        b'O' | b'o' => [0b010, 0b101, 0b101, 0b101, 0b010],
        b'P' | b'p' => [0b110, 0b101, 0b110, 0b100, 0b100],
        b'Q' | b'q' => [0b010, 0b101, 0b101, 0b111, 0b001],
        b'R' | b'r' => [0b110, 0b101, 0b110, 0b101, 0b101],
        b'S' | b's' => [0b011, 0b100, 0b010, 0b001, 0b110],
        b'T' | b't' => [0b111, 0b010, 0b010, 0b010, 0b010],
        b'U' | b'u' => [0b101, 0b101, 0b101, 0b101, 0b111],
        b'V' | b'v' => [0b101, 0b101, 0b101, 0b101, 0b010],
        b'W' | b'w' => [0b101, 0b101, 0b111, 0b111, 0b101],
        b'X' | b'x' => [0b101, 0b101, 0b010, 0b101, 0b101],
        b'Y' | b'y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        b'Z' | b'z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        b'-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        b':' => [0b000, 0b010, 0b000, 0b010, 0b000],
        b'.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        b',' => [0b000, 0b000, 0b000, 0b010, 0b100],
        _ => return None,
    })
}

pub fn draw_text(fb: &mut Framebuffer, x: i32, y: i32, text: &str, black: bool) {
    draw_text_scaled(fb, x, y, text, black, 1);
}

/// Same font, integer-scaled: each glyph pixel becomes a `scale`×`scale` block
/// and the advance becomes `4 * scale`. Handy for host overlays where the
/// 400×240 frame is upscaled.
pub fn draw_text_scaled(fb: &mut Framebuffer, x: i32, y: i32, text: &str, black: bool, scale: i32) {
    let scale = scale.max(1);
    let mut cx = x;
    for c in text.bytes() {
        if let Some(rows) = glyph(c) {
            for (row, bits) in rows.iter().enumerate() {
                for col in 0..3 {
                    if bits & (1 << (2 - col)) != 0 {
                        let gx = (cx + col * scale) as u32;
                        let gy = (y + row as i32 * scale) as u32;
                        for dy in 0..scale as u32 {
                            for dx in 0..scale as u32 {
                                fb.set_pixel(gx + dx, gy + dy, black);
                            }
                        }
                    }
                }
            }
        }
        cx += 4 * scale;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuffer::Framebuffer;

    #[test]
    fn scaled_text_expands_each_glyph_pixel() {
        let mut one = Framebuffer::default();
        draw_text(&mut one, 0, 0, "A", true);
        let mut two = Framebuffer::default();
        draw_text_scaled(&mut two, 0, 0, "A", true, 2);
        for y in 0..5 {
            for x in 0..3 {
                if one.get_pixel(x, y) {
                    for dy in 0..2 {
                        for dx in 0..2 {
                            assert!(two.get_pixel(x * 2 + dx, y * 2 + dy), "missing {x},{y}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn scaled_text_advances_by_scale() {
        let mut fb = Framebuffer::default();
        draw_text_scaled(&mut fb, 0, 0, "II", true, 3);
        assert!(fb.get_pixel(12, 0));
        assert!(!fb.get_pixel(11, 0));
    }
}
