use zipper_core::WorldMap;
fn main() {
    let m = WorldMap::from_path(WorldMap::data_file_path()).expect("map");
    let anims = [153u16, 245, 166, 168, 249, 170, 310, 314, 357];
    let mut by = std::collections::BTreeMap::<u16, Vec<(i32, i32)>>::new();
    for y in 0..m.height as i32 {
        for x in 0..m.width as i32 {
            let g = m.tile(x, y);
            if anims.contains(&g) {
                by.entry(g).or_default().push((x, y));
            }
        }
    }
    for (g, cells) in &by {
        println!("gid {g}: {} cells; sample {:?}", cells.len(), &cells[..cells.len().min(8)]);
    }
    println!("\nnear start (109,182 ±50), non-245:");
    for (g, cells) in &by {
        if *g == 245 {
            continue;
        }
        for &(x, y) in cells {
            if (x - 109).abs() < 50 && (y - 182).abs() < 50 {
                println!("  ({x},{y}) gid={g}");
            }
        }
    }
}
