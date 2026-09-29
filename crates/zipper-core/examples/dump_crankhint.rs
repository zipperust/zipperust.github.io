//! Dump crankhint.pdt cells to PPM for visual check.
use std::path::PathBuf;
fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let data = std::fs::read(root.join("www/assets/demo/crankhint.pdt")).expect("crankhint.pdt");
    let table = zipper_core::pdi::decode_pdt(&data).expect("decode");
    println!("cells={} per_row={}", table.len(), table.cells_per_row);
    for (i, cell) in table.cells.iter().enumerate() {
        println!("  cell[{}] {}x{} alpha={}", i, cell.width, cell.height, cell.alpha.is_some());
        // count black pixels
        let mut black = 0u32;
        let stride = zipper_core::bitmap::Bitmap::stride(cell.width);
        for y in 0..cell.height {
            for x in 0..cell.width {
                let bi = (y as usize) * stride + (x as usize) / 8;
                let bit = 7 - (x as usize % 8);
                let opaque = cell.alpha.as_ref().map(|a| (a[bi] >> bit) & 1 == 1).unwrap_or(true);
                let white = (cell.color[bi] >> bit) & 1 == 1;
                if opaque && !white { black += 1; }
            }
        }
        println!("    black_opaque={}", black);
    }
}
