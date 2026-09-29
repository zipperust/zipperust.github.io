use zipper_core::game::Game;
use zipper_core::WorldMap;

fn main() {
    let seed: u32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(840993252);
    let mut g = Game::new_with_seed(seed);
    g.load_worldmap(&std::fs::read(WorldMap::data_file_path()).expect("data/worldmap.bin"))
        .expect("load worldmap");
    println!("seed={seed}");
    println!("chest={:?}", g.chest_pos());
    println!("has_key={}", g.has_key());
    if let Some((x, y)) = g.chest_pos() {
        println!("tile={}", g.world.tile(x, y));
        // nearest exits/rooms for context
        let mut near: Vec<_> = g
            .world
            .exits
            .iter()
            .filter(|e| (e.x - x).abs() <= 16 && (e.y - y).abs() <= 16)
            .map(|e| {
                (
                    e.id,
                    e.x,
                    e.y,
                    e.dialog.as_str(),
                    e.kind,
                    e.face,
                    e.nx,
                    e.ny,
                    e.sx,
                    e.sy,
                )
            })
            .collect();
        near.sort_by_key(|t| (t.1 - x).abs() + (t.2 - y).abs());
        println!("nearby exits/npcs (id,x,y,dialog,kind,face,nx,ny,sx,sy):");
        for row in near.iter().take(20) {
            println!("  {row:?}");
        }
        println!("all chests:");
        for (i, c) in g.world.chests.iter().enumerate() {
            let d = (c.x - x).abs() + (c.y - y).abs();
            let mark = if c.x == x && c.y == y { " <--" } else { "" };
            println!("  [{i}] ({},{}) gid={} dist={d}{mark}", c.x, c.y, c.gid);
        }
    }
}
