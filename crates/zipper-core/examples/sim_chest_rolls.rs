//! Simulate chest pad rolls over many seeds (uniformity check for `SpawnRng`).
//!
//! ```text
//! cargo run -p zipper-core --example sim_chest_rolls
//! cargo run -p zipper-core --example sim_chest_rolls -- 10000
//! ```

use std::collections::BTreeMap;
use zipper_core::game::Game;
use zipper_core::WorldMap;

fn main() {
    let n_games: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);

    let map_bytes = std::fs::read(WorldMap::data_file_path()).expect("data/worldmap.bin");
    let mut probe = Game::new_with_seed(1);
    probe.load_worldmap(&map_bytes).expect("load worldmap");
    let pads: Vec<(i32, i32)> = probe.world.chests.iter().map(|c| (c.x, c.y)).collect();
    assert!(!pads.is_empty(), "map has no chest pads");

    println!("pads ({}):", pads.len());
    for (i, &(x, y)) in pads.iter().enumerate() {
        println!("  [{i}] ({x},{y}) {}", quadrant(x, y));
    }

    // Consecutive epoch-like seeds around the default demo seed.
    let base = zipper_core::DEFAULT_RANDOM_SEED;
    tally("consecutive seeds", base..base.saturating_add(n_games as u32), &map_bytes, &pads);

    // Spread u32 seeds (LCG of a fixed starter — not cryptographic, just spread).
    let mut spread = Vec::with_capacity(n_games);
    let mut s = 0xA5A5_1234u32;
    for _ in 0..n_games {
        s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        spread.push(s);
    }
    tally("spread u32 seeds", spread, &map_bytes, &pads);
}

fn tally(
    label: &str,
    seeds: impl IntoIterator<Item = u32>,
    map_bytes: &[u8],
    pads: &[(i32, i32)],
) {
    let mut by_idx: BTreeMap<usize, usize> = BTreeMap::new();
    let mut by_quad: BTreeMap<&str, usize> = BTreeMap::new();
    let mut total = 0usize;
    for seed in seeds {
        let mut g = Game::new_with_seed(seed);
        g.load_worldmap(map_bytes).expect("load worldmap");
        let Some((x, y)) = g.chest_pos() else {
            continue;
        };
        let idx = pads
            .iter()
            .position(|&p| p == (x, y))
            .expect("rolled chest must be a map pad");
        *by_idx.entry(idx).or_default() += 1;
        *by_quad.entry(quadrant(x, y)).or_default() += 1;
        total += 1;
    }

    println!("\n=== {label} (N={total}) ===");
    let exp = total as f64 / pads.len() as f64;
    let mut chi = 0.0;
    for i in 0..pads.len() {
        let (x, y) = pads[i];
        let c = *by_idx.get(&i).unwrap_or(&0);
        let bar = "#".repeat(c.min(80));
        println!(
            "  [{i}] ({x:3},{y:3}) {:2}  {c:4}  {bar}",
            quadrant(x, y)
        );
        if exp > 0.0 {
            let d = c as f64 - exp;
            chi += d * d / exp;
        }
    }
    println!("by quadrant: {by_quad:?}");
    println!("chi^2 vs uniform: {chi:.2} (df={}; ~0.05 critical ≈ 16.9 for df=9)", pads.len() - 1);

    let odd_hits: usize = by_idx
        .iter()
        .filter(|(i, _)| *i % 2 == 1)
        .map(|(_, c)| *c)
        .sum();
    let even_hits: usize = by_idx
        .iter()
        .filter(|(i, _)| *i % 2 == 0)
        .map(|(_, c)| *c)
        .sum();
    println!("even-index pads: {even_hits}  odd-index pads: {odd_hits}");
}

fn quadrant(x: i32, y: i32) -> &'static str {
    let ns = if y < 128 { "N" } else { "S" };
    let ew = if x < 128 { "W" } else { "E" };
    match (ns, ew) {
        ("N", "W") => "NW",
        ("N", "E") => "NE",
        ("S", "W") => "SW",
        _ => "SE",
    }
}
