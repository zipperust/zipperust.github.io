//! Packed 1-bit framebuffer (Playdate 400×240).

/// Logical Playdate display size.
pub const SCREEN_WIDTH: u32 = 400;
pub const SCREEN_HEIGHT: u32 = 240;
pub const FRAMEBUFFER_BYTES: usize =
    (SCREEN_WIDTH as usize * SCREEN_HEIGHT as usize) / 8;

/// Packed 1-bit framebuffer, row-major, MSB = leftmost pixel in a byte.
/// White = 0, black = 1 (ink).
#[derive(Clone)]
pub struct Framebuffer {
    pub pixels: [u8; FRAMEBUFFER_BYTES],
}

impl Default for Framebuffer {
    fn default() -> Self {
        Self {
            pixels: [0; FRAMEBUFFER_BYTES],
        }
    }
}

impl Framebuffer {
    pub fn clear(&mut self, black: bool) {
        let v = if black { 0xFF } else { 0x00 };
        self.pixels.fill(v);
    }

    pub fn set_pixel(&mut self, x: u32, y: u32, black: bool) {
        if x >= SCREEN_WIDTH || y >= SCREEN_HEIGHT {
            return;
        }
        let i = (y * SCREEN_WIDTH + x) as usize;
        let byte = i / 8;
        let bit = 7 - (i % 8);
        if black {
            self.pixels[byte] |= 1 << bit;
        } else {
            self.pixels[byte] &= !(1 << bit);
        }
    }

    pub fn get_pixel(&self, x: u32, y: u32) -> bool {
        if x >= SCREEN_WIDTH || y >= SCREEN_HEIGHT {
            return false;
        }
        let i = (y * SCREEN_WIDTH + x) as usize;
        let byte = i / 8;
        let bit = 7 - (i % 8);
        (self.pixels[byte] >> bit) & 1 == 1
    }

    pub fn hline(&mut self, x0: i32, x1: i32, y: i32, black: bool) {
        if y < 0 || y >= SCREEN_HEIGHT as i32 {
            return;
        }
        let (mut a, mut b) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
        a = a.max(0);
        b = b.min(SCREEN_WIDTH as i32 - 1);
        for x in a..=b {
            self.set_pixel(x as u32, y as u32, black);
        }
    }

    pub fn vline(&mut self, x: i32, y0: i32, y1: i32, black: bool) {
        if x < 0 || x >= SCREEN_WIDTH as i32 {
            return;
        }
        let (mut a, mut b) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
        a = a.max(0);
        b = b.min(SCREEN_HEIGHT as i32 - 1);
        for y in a..=b {
            self.set_pixel(x as u32, y as u32, black);
        }
    }

    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, black: bool) {
        if w <= 0 || h <= 0 {
            return;
        }
        self.hline(x, x + w - 1, y, black);
        self.hline(x, x + w - 1, y + h - 1, black);
        self.vline(x, y, y + h - 1, black);
        self.vline(x + w - 1, y, y + h - 1, black);
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, black: bool) {
        if w <= 0 || h <= 0 {
            return;
        }
        for row in y..y + h {
            self.hline(x, x + w - 1, row, black);
        }
    }

    /// Convex polygon fill (scanline). Used by room-transition diagonal wipes.
    pub fn fill_polygon(&mut self, verts: &[(i32, i32)], black: bool) {
        if verts.len() < 3 {
            return;
        }
        let mut y_min = i32::MAX;
        let mut y_max = i32::MIN;
        for &(_, y) in verts {
            y_min = y_min.min(y);
            y_max = y_max.max(y);
        }
        y_min = y_min.max(0);
        y_max = y_max.min(SCREEN_HEIGHT as i32 - 1);
        let n = verts.len();
        for y in y_min..=y_max {
            let mut nodes = Vec::with_capacity(n);
            for i in 0..n {
                let (x0, y0) = verts[i];
                let (x1, y1) = verts[(i + 1) % n];
                if (y0 < y && y1 >= y) || (y1 < y && y0 >= y) {
                    let dy = y1 - y0;
                    if dy != 0 {
                        let x = x0 + (y - y0) * (x1 - x0) / dy;
                        nodes.push(x);
                    }
                }
            }
            nodes.sort_unstable();
            let mut i = 0;
            while i + 1 < nodes.len() {
                self.hline(nodes[i], nodes[i + 1], y, black);
                i += 2;
            }
        }
    }

    /// Left-closing diagonal wipe (`main.lua` initbackground out wipe).
    /// Quad: (0,0)–(0,240)–(230,240)–(170,0).
    pub fn fill_transition_wipe_out(&mut self) {
        self.fill_polygon(
            &[
                (0, 0),
                (0, SCREEN_HEIGHT as i32),
                (SCREEN_WIDTH as i32 / 2 + 30, SCREEN_HEIGHT as i32),
                (SCREEN_WIDTH as i32 / 2 - 30, 0),
            ],
            true,
        );
    }

    /// Right-closing diagonal wipe (`main.lua` initbackground in wipe).
    /// Quad: (400,240)–(400,0)–(170,0)–(230,240).
    pub fn fill_transition_wipe_in(&mut self) {
        self.fill_polygon(
            &[
                (SCREEN_WIDTH as i32, SCREEN_HEIGHT as i32),
                (SCREEN_WIDTH as i32, 0),
                (SCREEN_WIDTH as i32 / 2 - 30, 0),
                (SCREEN_WIDTH as i32 / 2 + 30, SCREEN_HEIGHT as i32),
            ],
            true,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_wipe_out_covers_left() {
        let mut fb = Framebuffer::default();
        fb.fill_transition_wipe_out();
        assert!(fb.get_pixel(0, 120), "left edge must be black");
        assert!(fb.get_pixel(100, 120), "left half interior must be black");
        assert!(
            !fb.get_pixel(350, 120),
            "far right must stay clear after out wipe"
        );
    }

    #[test]
    fn transition_wipe_in_covers_right() {
        let mut fb = Framebuffer::default();
        fb.fill_transition_wipe_in();
        assert!(fb.get_pixel(399, 120), "right edge must be black");
        assert!(fb.get_pixel(300, 120), "right half interior must be black");
        assert!(
            !fb.get_pixel(50, 120),
            "far left must stay clear after in wipe"
        );
    }
}

/// Bresenham line.
pub fn draw_line(fb: &mut Framebuffer, x0: i32, y0: i32, x1: i32, y1: i32, black: bool) {
    let mut x0 = x0;
    let mut y0 = y0;
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        if x0 >= 0 && y0 >= 0 {
            fb.set_pixel(x0 as u32, y0 as u32, black);
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}
