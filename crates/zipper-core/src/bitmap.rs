//! 1-bit bitmaps with optional alpha mask (Playdate image cells).

use crate::framebuffer::{Framebuffer, SCREEN_HEIGHT, SCREEN_WIDTH};

/// Playdate `gfx.kDrawMode*` subset used by Zipper sprites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawMode {
    /// `gfx.kDrawModeCopy` — write source color where opaque.
    Copy,
    /// `gfx.kDrawModeNXOR` — flip dest under opaque black ink (selector path).
    Nxor,
    /// `gfx.kDrawModeXOR` — same flip semantics for our 1-bit path (blood spray).
    Xor,
    /// `gfx.kDrawModeInverted` — swap black/white of opaque source (`enemy:stun`).
    Inverted,
}

/// Owned 1-bit image. Color: 0 = black, 1 = white (Playdate). Alpha: 1 = opaque.
#[derive(Clone, Debug)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// Packed MSB-first, row stride = `(width + 7) / 8`.
    pub color: Vec<u8>,
    /// Same layout as `color` when present.
    pub alpha: Option<Vec<u8>>,
}

impl Bitmap {
    pub fn stride(width: u32) -> usize {
        ((width + 7) / 8) as usize
    }

    pub fn empty(width: u32, height: u32) -> Self {
        let n = Self::stride(width) * height as usize;
        Self {
            width,
            height,
            color: vec![0xFF; n], // white
            alpha: None,
        }
    }

    pub fn with_alpha(width: u32, height: u32) -> Self {
        let n = Self::stride(width) * height as usize;
        Self {
            width,
            height,
            color: vec![0xFF; n],
            alpha: Some(vec![0x00; n]), // fully transparent
        }
    }

    /// Fully transparent placeholder (empty PDT clip cells, e.g. tileset GID 1).
    pub fn transparent(width: u32, height: u32) -> Self {
        Self::with_alpha(width, height)
    }

    #[inline]
    fn bit_at(data: &[u8], width: u32, x: u32, y: u32) -> bool {
        let stride = Self::stride(width);
        let byte = y as usize * stride + (x as usize / 8);
        let bit = 7 - (x as usize % 8);
        (data[byte] >> bit) & 1 == 1
    }

    #[inline]
    fn set_bit(data: &mut [u8], width: u32, x: u32, y: u32, one: bool) {
        let stride = Self::stride(width);
        let byte = y as usize * stride + (x as usize / 8);
        let bit = 7 - (x as usize % 8);
        if one {
            data[byte] |= 1 << bit;
        } else {
            data[byte] &= !(1 << bit);
        }
    }

    /// Color bit: true = white, false = black.
    pub fn color_at(&self, x: u32, y: u32) -> bool {
        if x >= self.width || y >= self.height {
            return true;
        }
        Self::bit_at(&self.color, self.width, x, y)
    }

    pub fn opaque_at(&self, x: u32, y: u32) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        match &self.alpha {
            Some(a) => Self::bit_at(a, self.width, x, y),
            None => true,
        }
    }

    pub fn set_pixel(&mut self, x: u32, y: u32, white: bool, opaque: bool) {
        if x >= self.width || y >= self.height {
            return;
        }
        Self::set_bit(&mut self.color, self.width, x, y, white);
        if let Some(a) = self.alpha.as_mut() {
            Self::set_bit(a, self.width, x, y, opaque);
        }
    }

    /// Blit onto the framebuffer. Black ink from the bitmap is drawn where opaque.
    /// White opaque pixels clear ink (draw white). Transparent pixels are skipped.
    /// Matches Playdate `gfx.kDrawModeCopy`.
    pub fn blit(&self, fb: &mut Framebuffer, dx: i32, dy: i32) {
        self.blit_mode(fb, dx, dy, DrawMode::Copy);
    }

    /// Blit with a Playdate image draw mode (`selector` walkicons use NXOR).
    pub fn blit_mode(&self, fb: &mut Framebuffer, dx: i32, dy: i32, mode: DrawMode) {
        self.blit_mode_ex(fb, dx, dy, mode, false);
    }

    /// Like [`blit_mode`], optionally mirroring on X (`gfx.kImageFlippedX`).
    pub fn blit_mode_flip_x(&self, fb: &mut Framebuffer, dx: i32, dy: i32, mode: DrawMode) {
        self.blit_mode_ex(fb, dx, dy, mode, true);
    }

    fn blit_mode_ex(
        &self,
        fb: &mut Framebuffer,
        dx: i32,
        dy: i32,
        mode: DrawMode,
        flip_x: bool,
    ) {
        let w = self.width as i32;
        let h = self.height as i32;
        // Clip the sprite to the destination once, so the inner loop needs no
        // per-pixel bounds checks.
        let x0 = (-dx).max(0);
        let x1 = (SCREEN_WIDTH as i32 - dx).min(w);
        let y0 = (-dy).max(0);
        let y1 = (SCREEN_HEIGHT as i32 - dy).min(h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }

        let sstride = Self::stride(self.width);
        let dstride = (SCREEN_WIDTH / 8) as usize;
        let alpha = self.alpha.as_deref();

        for y in y0..y1 {
            let srow = y as usize * sstride;
            let drow = (dy + y) as usize * dstride;
            for x in x0..x1 {
                let sbit = if flip_x { (w - 1 - x) as usize } else { x as usize };
                let dbit = (dx + x) as usize;
                let sbyte = srow + (sbit >> 3);
                let smask = 0x80u8 >> (sbit & 7);
                let opaque = alpha.map_or(true, |a| a[sbyte] & smask != 0);
                if opaque {
                    let src_black = self.color[sbyte] & smask == 0;
                    let dbyte = drow + (dbit >> 3);
                    let dmask = 0x80u8 >> (dbit & 7);
                    match mode {
                        DrawMode::Copy => {
                            if src_black {
                                fb.pixels[dbyte] |= dmask;
                            } else {
                                fb.pixels[dbyte] &= !dmask;
                            }
                        }
                        DrawMode::Inverted => {
                            if src_black {
                                fb.pixels[dbyte] &= !dmask;
                            } else {
                                fb.pixels[dbyte] |= dmask;
                            }
                        }
                        DrawMode::Nxor | DrawMode::Xor => {
                            if src_black {
                                fb.pixels[dbyte] ^= dmask;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Original per-pixel blit, kept as the parity reference for tests.
    #[cfg(test)]
    fn blit_mode_ex_ref(
        &self,
        fb: &mut Framebuffer,
        dx: i32,
        dy: i32,
        mode: DrawMode,
        flip_x: bool,
    ) {
        for y in 0..self.height {
            for x in 0..self.width {
                let sx = if flip_x {
                    self.width - 1 - x
                } else {
                    x
                };
                if !self.opaque_at(sx, y) {
                    continue;
                }
                let px = dx + x as i32;
                let py = dy + y as i32;
                if px < 0 || py < 0 {
                    continue;
                }
                let src_white = self.color_at(sx, y);
                let src_black = !src_white;
                let ux = px as u32;
                let uy = py as u32;
                match mode {
                    DrawMode::Copy => fb.set_pixel(ux, uy, src_black),
                    DrawMode::Inverted => fb.set_pixel(ux, uy, !src_black),
                    DrawMode::Nxor | DrawMode::Xor => {
                        if src_black {
                            let dest_black = fb.get_pixel(ux, uy);
                            fb.set_pixel(ux, uy, !dest_black);
                        }
                    }
                }
            }
        }
    }

    /// Expand to grayscale+alpha PNG-style bytes for tooling (width*height*2).
    pub fn to_luma_alpha(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity((self.width * self.height * 2) as usize);
        for y in 0..self.height {
            for x in 0..self.width {
                let white = self.color_at(x, y);
                let opaque = self.opaque_at(x, y);
                out.push(if white { 255 } else { 0 });
                out.push(if opaque { 255 } else { 0 });
            }
        }
        out
    }
}

/// Image table (`.pdt`): ordered list of cells, optional matrix layout.
#[derive(Clone, Debug)]
pub struct ImageTable {
    pub cells: Vec<Bitmap>,
    /// Cells per row for matrix tables; equals `cells.len()` for sequential.
    pub cells_per_row: u16,
}

impl ImageTable {
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Bitmap> {
        self.cells.get(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic xorshift so failures are reproducible.
    struct Rng(u32);
    impl Rng {
        fn next(&mut self) -> u32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.0 = x;
            x
        }
        fn byte(&mut self) -> u8 {
            self.next() as u8
        }
    }

    fn random_bitmap(rng: &mut Rng, w: u32, h: u32, with_alpha: bool) -> Bitmap {
        let n = Bitmap::stride(w) * h as usize;
        let mut color = vec![0u8; n];
        for b in &mut color {
            *b = rng.byte();
        }
        let alpha = with_alpha.then(|| {
            let mut a = vec![0u8; n];
            for b in &mut a {
                *b = rng.byte();
            }
            a
        });
        Bitmap {
            width: w,
            height: h,
            color,
            alpha,
        }
    }

    #[test]
    fn fast_blit_matches_reference() {
        let modes = [
            DrawMode::Copy,
            DrawMode::Inverted,
            DrawMode::Nxor,
            DrawMode::Xor,
        ];
        let mut rng = Rng(0x1234_5678);
        for case in 0..64 {
            let w = 1 + rng.next() % 40;
            let h = 1 + rng.next() % 40;
            let with_alpha = case % 2 == 0;
            let bmp = random_bitmap(&mut rng, w, h, with_alpha);
            // Offsets straddling all four edges, plus interior positions.
            let dx = (rng.next() % 500) as i32 - 50;
            let dy = (rng.next() % 300) as i32 - 40;
            for &mode in &modes {
                for &flip in &[false, true] {
                    let mut a = Framebuffer::default();
                    let mut b = Framebuffer::default();
                    bmp.blit_mode_ex(&mut a, dx, dy, mode, flip);
                    bmp.blit_mode_ex_ref(&mut b, dx, dy, mode, flip);
                    assert_eq!(
                        a.pixels, b.pixels,
                        "case {case} w={w} h={h} alpha={with_alpha} dx={dx} dy={dy} \
                         mode={mode:?} flip={flip}"
                    );
                }
            }
        }
    }

    /// Seed the destination with noise so XOR/NXOR read a non-trivial background.
    #[test]
    fn fast_blit_matches_reference_on_noisy_dest() {
        let mut rng = Rng(0xDEAD_BEEF);
        for case in 0..32 {
            let w = 1 + rng.next() % 50;
            let h = 1 + rng.next() % 50;
            let bmp = random_bitmap(&mut rng, w, h, case % 2 == 0);
            let dx = (rng.next() % 500) as i32 - 60;
            let dy = (rng.next() % 300) as i32 - 50;
            for &mode in &[DrawMode::Nxor, DrawMode::Xor, DrawMode::Copy] {
                for &flip in &[false, true] {
                    let mut seed = vec![0u8; SCREEN_WIDTH as usize * SCREEN_HEIGHT as usize / 8];
                    for b in &mut seed {
                        *b = rng.byte();
                    }
                    let mut a = Framebuffer::default();
                    a.pixels.copy_from_slice(&seed);
                    let mut b = Framebuffer::default();
                    b.pixels.copy_from_slice(&seed);
                    bmp.blit_mode_ex(&mut a, dx, dy, mode, flip);
                    bmp.blit_mode_ex_ref(&mut b, dx, dy, mode, flip);
                    assert_eq!(
                        a.pixels, b.pixels,
                        "case {case} w={w} h={h} dx={dx} dy={dy} mode={mode:?} flip={flip}"
                    );
                }
            }
        }
    }
}
