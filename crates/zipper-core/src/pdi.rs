//! Playdate `.pdi` / `.pdt` decoders (see cranksters reverse-engineering docs).

use crate::bitmap::{Bitmap, ImageTable};

/// Decode errors for Playdate image formats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    BadMagic,
    Truncated,
    Inflate,
    BadCell,
    EmptyTable,
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecodeError::BadMagic => write!(f, "bad magic"),
            DecodeError::Truncated => write!(f, "truncated data"),
            DecodeError::Inflate => write!(f, "zlib inflate failed"),
            DecodeError::BadCell => write!(f, "bad image cell"),
            DecodeError::EmptyTable => write!(f, "empty image table"),
        }
    }
}

impl std::error::Error for DecodeError {}

const PDI_MAGIC: &[u8; 12] = b"Playdate IMG";
const PDT_MAGIC: &[u8; 12] = b"Playdate IMT";
const FLAG_COMPRESSED: u32 = 0x8000_0000;

fn read_u16(data: &[u8], off: usize) -> Result<u16, DecodeError> {
    let b = data.get(off..off + 2).ok_or(DecodeError::Truncated)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn read_u32(data: &[u8], off: usize) -> Result<u32, DecodeError> {
    let b = data.get(off..off + 4).ok_or(DecodeError::Truncated)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Payload after optional compression header.
fn payload_after_header(data: &[u8], magic: &[u8; 12]) -> Result<Vec<u8>, DecodeError> {
    if data.len() < 16 || &data[0..12] != magic {
        return Err(DecodeError::BadMagic);
    }
    let flags = read_u32(data, 12)?;
    if flags & FLAG_COMPRESSED != 0 {
        if data.len() < 32 {
            return Err(DecodeError::Truncated);
        }
        // size, width, height, reserved/count
        let _size = read_u32(data, 16)?;
        miniz_oxide::inflate::decompress_to_vec_zlib(&data[32..]).map_err(|_| DecodeError::Inflate)
    } else {
        Ok(data[16..].to_vec())
    }
}

/// Decode one image cell starting at `offset` into a tight-packed Bitmap.
/// Returns (bitmap, bytes_consumed).
pub fn decode_cell(data: &[u8], offset: usize) -> Result<(Bitmap, usize), DecodeError> {
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
        pos += color_size;
        Some(a)
    } else {
        None
    };

    let full_w = clip_left + clip_w + clip_right;
    let full_h = clip_top + clip_h + clip_bottom;
    if full_w == 0 || full_h == 0 {
        return Err(DecodeError::BadCell);
    }

    // Empty clip (e.g. tileset GID 1 / cell 0): pad-only cell with no ink.
    // Must stay fully transparent — `Bitmap::empty` is opaque white and paints
    // solid rectangles when blitted (`main.lua` draws `tiles[tdata]` for tdata≠0).
    if clip_w == 0 || clip_h == 0 {
        return Ok((Bitmap::transparent(full_w, full_h), pos - offset));
    }

    let mut bmp = if has_alpha {
        Bitmap::with_alpha(full_w, full_h)
    } else {
        Bitmap::empty(full_w, full_h)
    };

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
            let x = clip_left + cx;
            let y = clip_top + cy;
            bmp.set_pixel(x, y, white, opaque);
        }
    }

    Ok((bmp, pos - offset))
}

/// Decode a standalone `.pdi` file.
pub fn decode_pdi(data: &[u8]) -> Result<Bitmap, DecodeError> {
    let payload = payload_after_header(data, PDI_MAGIC)?;
    let (bmp, _) = decode_cell(&payload, 0)?;
    Ok(bmp)
}

/// Decode a `.pdt` image table.
pub fn decode_pdt(data: &[u8]) -> Result<ImageTable, DecodeError> {
    let payload = payload_after_header(data, PDT_MAGIC)?;
    if payload.len() < 4 {
        return Err(DecodeError::Truncated);
    }
    let num_cells = read_u16(&payload, 0)? as usize;
    let cells_per_row = read_u16(&payload, 2)?;
    if num_cells == 0 {
        return Err(DecodeError::EmptyTable);
    }

    // Offset table: (num_cells - 1) cell offsets + 1 end offset = num_cells u32s
    // relative to end of table. First cell starts at end of table.
    let table_end = 4 + num_cells * 4;
    if payload.len() < table_end {
        return Err(DecodeError::Truncated);
    }

    let mut offsets = Vec::with_capacity(num_cells + 1);
    offsets.push(0u32); // first cell at 0 relative to table end
    for i in 0..num_cells {
        let off = read_u32(&payload, 4 + i * 4)?;
        offsets.push(off);
    }

    let base = table_end;
    let mut cells = Vec::with_capacity(num_cells);
    for i in 0..num_cells {
        let start = base + offsets[i] as usize;
        // Prefer walking cells; offsets may point at cell starts
        if start >= payload.len() {
            return Err(DecodeError::Truncated);
        }
        let (bmp, _consumed) = decode_cell(&payload, start)?;
        cells.push(bmp);
    }

    Ok(ImageTable {
        cells,
        cells_per_row,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_key_pdi_if_present() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ref/Zipper.pdx/Images/key.pdi"
        );
        let Ok(data) = std::fs::read(path) else {
            // ref/ may be gitignored / absent in CI
            return;
        };
        let bmp = decode_pdi(&data).expect("key.pdi");
        assert_eq!((bmp.width, bmp.height), (16, 16));
        assert!(bmp.alpha.is_some());
    }

    #[test]
    fn decode_playerface_pdi_if_present() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ref/Zipper.pdx/Images/playerface.pdi"
        );
        let Ok(data) = std::fs::read(path) else {
            return;
        };
        let bmp = decode_pdi(&data).expect("playerface.pdi");
        assert_eq!((bmp.width, bmp.height), (64, 32));
    }

    #[test]
    fn tileset_cell0_is_transparent_not_opaque_white() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ref/Zipper.pdx/Images/tileset.pdt"
        );
        let Ok(data) = std::fs::read(path) else {
            return;
        };
        let table = decode_pdt(&data).expect("tileset.pdt");
        let cell0 = table.get(0).expect("cell 0");
        assert_eq!((cell0.width, cell0.height), (32, 64));
        // Empty clip must not paint: every pixel transparent.
        for y in 0..cell0.height {
            for x in 0..cell0.width {
                assert!(
                    !cell0.opaque_at(x, y),
                    "cell0 opaque at ({x},{y}) — would draw white artefacts for GID 1"
                );
            }
        }
    }

    /// Lua draws `Images/unifiedtiles` (512). Castle floors use GIDs 257+;
    /// loading only `tileset.pdt` (256) leaves interiors black.
    #[test]
    fn unifiedtiles_covers_castle_gids() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ref/Zipper.pdx/Images/unifiedtiles.pdt"
        );
        let Ok(data) = std::fs::read(path) else {
            return;
        };
        let table = decode_pdt(&data).expect("unifiedtiles.pdt");
        assert!(
            table.len() >= 284,
            "unifiedtiles must cover castle GIDs, got {} cells",
            table.len()
        );
        for gid in [258u16, 259, 269, 273, 284] {
            let cell = table
                .get((gid as usize) - 1)
                .unwrap_or_else(|| panic!("missing GID {gid}"));
            let opaque = (0..cell.height)
                .flat_map(|y| (0..cell.width).map(move |x| (x, y)))
                .filter(|&(x, y)| cell.opaque_at(x, y))
                .count();
            assert!(opaque > 0, "castle GID {gid} must have opaque pixels");
        }
    }

    #[test]
    fn decode_hole_pdt_if_present() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ref/Zipper.pdx/Images/hole.pdt"
        );
        let Ok(data) = std::fs::read(path) else {
            return;
        };
        let table = decode_pdt(&data).expect("hole.pdt");
        assert!(!table.is_empty());
        assert_eq!(table.cells_per_row as usize, table.len().min(table.cells_per_row as usize));
    }
}
