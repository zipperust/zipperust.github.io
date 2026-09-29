//! Top-down reference PNG of the outdoor `worldmap.bin` grid.
//!
//! Same `(tile_x, tile_y)` system as `#god` HUD / chests / exits — not screen pixels.
//! Y increases downward (row-major export order).
//!
//! “Rooms” here = connected floor/NPC components with **doors treated as barriers**
//! (combat-room feel). Soft alternating floor tints + dark outlines where a room
//! meets wall / void / door. No arbitrary lattice.
//!
//! ```text
//! cargo run -p zipper-core --example dump_worldmap
//! # → docs/worldmap-overview.png (4×) and docs/worldmap-overview-1x.png
//! ```

use std::collections::VecDeque;
use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use zipper_core::level::Terrain;
use zipper_core::worldmap::{WorldMap, START_PLAY};

const SCALE: u32 = 4;
const LEGEND_H: u32 = 56;

fn main() {
    let map = WorldMap::from_path(WorldMap::data_file_path()).expect("data/worldmap.bin");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let docs = root.join("docs");
    std::fs::create_dir_all(&docs).expect("docs/");

    let rooms = label_rooms(&map);
    eprintln!(
        "door-separated rooms: {} (floors/NPC; doors are boundaries)",
        rooms.n_rooms
    );

    let overview = render(&map, &rooms, SCALE, true);
    let out = docs.join("worldmap-overview.png");
    write_rgb_png(&out, overview.width, overview.height, &overview.rgb).expect("write overview");
    eprintln!(
        "wrote {} ({}×{}, {}px/tile)",
        out.display(),
        overview.width,
        overview.height,
        SCALE
    );

    let tiny = render(&map, &rooms, 1, false);
    let out1 = docs.join("worldmap-overview-1x.png");
    write_rgb_png(&out1, tiny.width, tiny.height, &tiny.rgb).expect("write 1x");
    eprintln!("wrote {} ({}×{})", out1.display(), tiny.width, tiny.height);

    eprintln!(
        "legend: tinted rooms | wall=gray door=orange void=black | chest=red exit=cyan monk=blue start=lime"
    );
    eprintln!(
        "god HUD coords match tiles: start play = {:?}; seed chests are the red squares",
        START_PLAY
    );
}

struct Image {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

/// Per-tile room id (`0` = not a room floor: wall/void/door).
struct RoomLabels {
    width: u32,
    /// 0 = non-room; 1..=n_rooms = component id.
    ids: Vec<u16>,
    n_rooms: u16,
}

fn label_rooms(map: &WorldMap) -> RoomLabels {
    let w = map.width as usize;
    let h = map.height as usize;
    let mut ids = vec![0u16; w * h];
    let mut next = 1u16;
    let mut q = VecDeque::new();

    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if ids[i] != 0 {
                continue;
            }
            if !is_room_floor(map.terrain_at(x as i32, y as i32)) {
                continue;
            }
            let id = next;
            next = next.saturating_add(1);
            ids[i] = id;
            q.clear();
            q.push_back((x, y));
            while let Some((cx, cy)) = q.pop_front() {
                for (dx, dy) in [(1isize, 0), (-1, 0), (0, 1), (0, -1)] {
                    let nx = cx as isize + dx;
                    let ny = cy as isize + dy;
                    if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                        continue;
                    }
                    let ni = ny as usize * w + nx as usize;
                    if ids[ni] != 0 {
                        continue;
                    }
                    if !is_room_floor(map.terrain_at(nx as i32, ny as i32)) {
                        continue;
                    }
                    ids[ni] = id;
                    q.push_back((nx as usize, ny as usize));
                }
            }
        }
    }

    RoomLabels {
        width: map.width as u32,
        ids,
        n_rooms: next.saturating_sub(1),
    }
}

fn is_room_floor(t: Terrain) -> bool {
    matches!(t, Terrain::Floor { .. } | Terrain::Npc { .. })
}

fn room_id(rooms: &RoomLabels, x: u32, y: u32) -> u16 {
    rooms.ids[(y * rooms.width + x) as usize]
}

fn render(map: &WorldMap, rooms: &RoomLabels, scale: u32, with_legend: bool) -> Image {
    let mw = map.width as u32;
    let mh = map.height as u32;
    let legend = if with_legend { LEGEND_H } else { 0 };
    let width = mw * scale;
    let height = mh * scale + legend;
    let mut rgb = vec![0u8; (width * height * 3) as usize];

    for y in 0..mh {
        for x in 0..mw {
            let t = map.terrain_at(x as i32, y as i32);
            let c = match t {
                Terrain::Floor { .. } | Terrain::Npc { .. } => {
                    room_tint(room_id(rooms, x, y), t)
                }
                other => terrain_color(other),
            };
            fill_cell(&mut rgb, width, x, y, scale, c);
        }
    }

    // Dark outline where a room cell meets wall / void / door / another room.
    let edge = [22, 22, 28];
    for y in 0..mh {
        for x in 0..mw {
            let id = room_id(rooms, x, y);
            if id == 0 {
                continue;
            }
            let neighbors = [
                (x.wrapping_sub(1), y, x > 0),
                (x + 1, y, x + 1 < mw),
                (x, y.wrapping_sub(1), y > 0),
                (x, y + 1, y + 1 < mh),
            ];
            for (nx, ny, ok) in neighbors {
                let border = if !ok {
                    true
                } else {
                    let nid = room_id(rooms, nx, ny);
                    nid != id
                };
                if border {
                    stroke_edge(&mut rgb, width, x, y, scale, nx as i32 - x as i32, ny as i32 - y as i32, edge);
                }
            }
        }
    }

    // Exits / NPC pads (under chests so chests win on overlap).
    for ex in &map.exits {
        let (x, y) = (ex.x as u32, ex.y as u32);
        if x >= mw || y >= mh {
            continue;
        }
        let color = if ex.kind == 1 {
            [70, 130, 255] // monk / npc object
        } else {
            [40, 220, 230] // door exit object
        };
        mark_diamond(&mut rgb, width, x, y, scale, color);
    }

    for c in &map.chests {
        let (x, y) = (c.x as u32, c.y as u32);
        if x >= mw || y >= mh {
            continue;
        }
        mark_square(&mut rgb, width, x, y, scale, [255, 48, 48], 1);
    }

    mark_cross(
        &mut rgb,
        width,
        START_PLAY.0 as u32,
        START_PLAY.1 as u32,
        scale,
        [80, 255, 80],
    );

    if with_legend {
        draw_legend(&mut rgb, width, mh * scale, scale, rooms.n_rooms);
    }

    Image { width, height, rgb }
}

/// Soft pastel palette hashed by room id so neighbors usually differ.
fn room_tint(id: u16, t: Terrain) -> [u8; 3] {
    // Distinct but muted hues — readable on a dark void.
    const PALETTE: [[u8; 3]; 12] = [
        [196, 178, 132], // sand
        [168, 186, 150], // sage
        [186, 168, 196], // lilac
        [150, 178, 196], // steel
        [196, 160, 140], // clay
        [160, 196, 178], // mint
        [196, 186, 150], // straw
        [178, 150, 168], // mauve
        [150, 168, 178], // slate
        [186, 150, 140], // rose-dust
        [168, 196, 160], // leaf
        [178, 178, 150], // olive wash
    ];
    let base = PALETTE[(id as usize).wrapping_mul(7) % PALETTE.len()];
    // NPC pads slightly brighter so they still read before the blue diamond.
    if matches!(t, Terrain::Npc { .. }) {
        [
            base[0].saturating_add(18),
            base[1].saturating_add(10),
            base[2].saturating_add(24),
        ]
    } else {
        base
    }
}

fn terrain_color(t: Terrain) -> [u8; 3] {
    match t {
        Terrain::Void => [18, 18, 22],
        Terrain::Floor { .. } => [170, 162, 130], // unused when rooms labeled
        Terrain::Wall { .. } => [92, 92, 98],
        Terrain::Door { .. } => [240, 160, 40],
        Terrain::Npc { .. } => [170, 90, 220],
    }
}

/// Paint the 1px-scale edge of cell `(tx,ty)` facing neighbor offset `(ox,oy)`.
fn stroke_edge(
    rgb: &mut [u8],
    width: u32,
    tx: u32,
    ty: u32,
    scale: u32,
    ox: i32,
    oy: i32,
    c: [u8; 3],
) {
    let x0 = tx * scale;
    let y0 = ty * scale;
    if scale == 1 {
        // At 1×, darken the whole border cell slightly toward the neighbor.
        put(rgb, width, tx, ty, mix(get(rgb, width, tx, ty).unwrap_or(c), c, 0.45));
        return;
    }
    if ox < 0 {
        for dy in 0..scale {
            put(rgb, width, x0, y0 + dy, c);
        }
    } else if ox > 0 {
        for dy in 0..scale {
            put(rgb, width, x0 + scale - 1, y0 + dy, c);
        }
    } else if oy < 0 {
        for dx in 0..scale {
            put(rgb, width, x0 + dx, y0, c);
        }
    } else if oy > 0 {
        for dx in 0..scale {
            put(rgb, width, x0 + dx, y0 + scale - 1, c);
        }
    }
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    [
        (a[0] as f32 * (1.0 - t) + b[0] as f32 * t) as u8,
        (a[1] as f32 * (1.0 - t) + b[1] as f32 * t) as u8,
        (a[2] as f32 * (1.0 - t) + b[2] as f32 * t) as u8,
    ]
}

fn get(rgb: &[u8], width: u32, x: u32, y: u32) -> Option<[u8; 3]> {
    let height = (rgb.len() / 3) as u32 / width;
    if x >= width || y >= height {
        return None;
    }
    let i = ((y * width + x) * 3) as usize;
    Some([rgb[i], rgb[i + 1], rgb[i + 2]])
}

fn fill_cell(rgb: &mut [u8], width: u32, tx: u32, ty: u32, scale: u32, c: [u8; 3]) {
    for dy in 0..scale {
        for dx in 0..scale {
            put(rgb, width, tx * scale + dx, ty * scale + dy, c);
        }
    }
}

fn mark_square(rgb: &mut [u8], width: u32, tx: u32, ty: u32, scale: u32, c: [u8; 3], pad: u32) {
    let x0 = tx * scale;
    let y0 = ty * scale;
    let s = scale.max(2);
    for dy in pad..s.saturating_sub(pad).max(pad + 1) {
        for dx in pad..s.saturating_sub(pad).max(pad + 1) {
            put(rgb, width, x0 + dx, y0 + dy, c);
        }
    }
}

fn mark_diamond(rgb: &mut [u8], width: u32, tx: u32, ty: u32, scale: u32, c: [u8; 3]) {
    let cx = tx * scale + scale / 2;
    let cy = ty * scale + scale / 2;
    let r = (scale as i32 / 2).max(1);
    for dy in -r..=r {
        for dx in -r..=r {
            if dx.abs() + dy.abs() <= r {
                put(
                    rgb,
                    width,
                    (cx as i32 + dx) as u32,
                    (cy as i32 + dy) as u32,
                    c,
                );
            }
        }
    }
}

fn mark_cross(rgb: &mut [u8], width: u32, tx: u32, ty: u32, scale: u32, c: [u8; 3]) {
    let cx = tx * scale + scale / 2;
    let cy = ty * scale + scale / 2;
    let arm = (scale * 2).max(4);
    for i in 0..arm {
        put(rgb, width, cx.saturating_sub(arm / 2) + i, cy, c);
        put(rgb, width, cx, cy.saturating_sub(arm / 2) + i, c);
    }
    mark_square(rgb, width, tx, ty, scale, c, 0);
}

fn draw_legend(rgb: &mut [u8], width: u32, y0: u32, scale: u32, n_rooms: u16) {
    for y in y0..(y0 + LEGEND_H) {
        for x in 0..width {
            put(rgb, width, x, y, [12, 12, 16]);
        }
    }
    let samples: &[(&str, [u8; 3])] = &[
        ("room", [196, 178, 132]),
        ("wall", [92, 92, 98]),
        ("door", [240, 160, 40]),
        ("void", [18, 18, 22]),
        ("chest", [255, 48, 48]),
        ("exit", [40, 220, 230]),
        ("monk", [70, 130, 255]),
        ("start", [80, 255, 80]),
    ];
    let mut x = 8u32;
    let y = y0 + 10;
    for (label, color) in samples {
        for dy in 0..12u32 {
            for dx in 0..12u32 {
                put(rgb, width, x + dx, y + dy, *color);
            }
        }
        draw_label(rgb, width, x + 16, y + 3, label);
        x += 16 + (label.len() as u32) * 4 + 18;
        if x + 80 > width {
            break;
        }
    }
    let note = format!(
        "{} door-split rooms  |  tile (x,y)=pixel/{}  |  y down  |  worldmap.bin",
        n_rooms, scale
    );
    draw_label(rgb, width, 8, y0 + 32, &note);
}

fn draw_label(rgb: &mut [u8], width: u32, x: u32, y: u32, text: &str) {
    let mut cx = x as i32;
    for b in text.bytes() {
        if let Some(rows) = glyph(b) {
            for (row, bits) in rows.iter().enumerate() {
                for col in 0..3 {
                    if bits & (1 << (2 - col)) != 0 {
                        put(
                            rgb,
                            width,
                            (cx + col) as u32,
                            y + row as u32,
                            [220, 220, 220],
                        );
                    }
                }
            }
        }
        cx += 4;
    }
}

fn glyph(c: u8) -> Option<[u8; 5]> {
    Some(match c {
        b' ' => [0, 0, 0, 0, 0],
        b'-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        b'/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        b'(' => [0b011, 0b100, 0b100, 0b100, 0b011],
        b')' => [0b110, 0b001, 0b001, 0b001, 0b110],
        b',' => [0b000, 0b000, 0b000, 0b010, 0b100],
        b'.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        b':' => [0b000, 0b010, 0b000, 0b010, 0b000],
        b'|' => [0b010, 0b010, 0b010, 0b010, 0b010],
        b'=' => [0b000, 0b111, 0b000, 0b111, 0b000],
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
        b'a' | b'A' => [0b010, 0b101, 0b111, 0b101, 0b101],
        b'b' | b'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        b'c' | b'C' => [0b011, 0b100, 0b100, 0b100, 0b011],
        b'd' | b'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        b'e' | b'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
        b'f' | b'F' => [0b111, 0b100, 0b110, 0b100, 0b100],
        b'g' | b'G' => [0b011, 0b100, 0b101, 0b101, 0b011],
        b'h' | b'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        b'i' | b'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        b'k' | b'K' => [0b101, 0b101, 0b110, 0b101, 0b101],
        b'l' | b'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        b'm' | b'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        b'n' | b'N' => [0b101, 0b111, 0b111, 0b111, 0b101],
        b'o' | b'O' => [0b010, 0b101, 0b101, 0b101, 0b010],
        b'p' | b'P' => [0b110, 0b101, 0b110, 0b100, 0b100],
        b'r' | b'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        b's' | b'S' => [0b011, 0b100, 0b010, 0b001, 0b110],
        b't' | b'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        b'u' | b'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
        b'v' | b'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
        b'w' | b'W' => [0b101, 0b101, 0b111, 0b111, 0b101],
        b'x' | b'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        b'y' | b'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        b'z' | b'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        _ => return None,
    })
}

fn put(rgb: &mut [u8], width: u32, x: u32, y: u32, c: [u8; 3]) {
    let height = (rgb.len() / 3) as u32 / width;
    if x >= width || y >= height {
        return;
    }
    let i = ((y * width + x) * 3) as usize;
    rgb[i] = c[0];
    rgb[i + 1] = c[1];
    rgb[i + 2] = c[2];
}

fn write_rgb_png(path: &std::path::Path, width: u32, height: u32, rgb: &[u8]) -> Result<(), String> {
    let file = File::create(path).map_err(|e| e.to_string())?;
    let w = BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgb).map_err(|e| e.to_string())?;
    Ok(())
}
